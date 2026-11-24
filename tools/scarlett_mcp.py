#!/usr/bin/env python3
"""Run and inspect an offline Project Scarlett session over MCP stdio."""
import argparse
import base64
from collections import deque
from contextlib import contextmanager
import ctypes as C
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from tempfile import mkdtemp
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
KEYS = {
    "a": 0xff0d,  # Enter
    "b": 0xff1b,  # Escape
    "x": ord("c"),
    "y": ord("v"),
    "up": ord("i"),
    "down": ord("k"),
    "left": ord("j"),
    "right": ord("l"),
}
WINDOW_ID = re.compile(r"0x[0-9a-f]+")
MILESTONES = re.compile(
    r"PresentX|sample #\d+ res=|display writes recorded|recorded raster draw|"
    r"screen-rectangle pipeline|XUserAddAsync|pipeline stream .* failed|"
    r"device lost|DXGI_ERROR_DEVICE|breakpoint|Game exited|Launch failed",
    re.IGNORECASE,
)


def tool(name, description, properties=None, required=()):
    return {
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties or {},
            "required": list(required),
            "additionalProperties": False,
        },
    }


TOOLS = [
    tool("launch", "Start GTA VI offline with MangoHud and periodic GPU readback.", {
        "trace_interval": {"type": "integer", "minimum": 0, "maximum": 36000, "default": 600},
        "profile": {"type": "string", "enum": ["packaged", "series-s"], "default": "packaged"},
        "trace_draws": {"type": "boolean", "default": False},
        "primary_plane_only": {"type": "boolean", "default": False},
    }),
    tool("status", "Read the launch state, recent log lines, graphics milestones, and frame telemetry.", {
        "lines": {"type": "integer", "minimum": 0, "maximum": 100, "default": 20},
    }),
    tool("screenshot", "Capture the game window as a PNG and save it with the run."),
    tool("press", "Press a virtual controller button on the game window, optionally focusing it first.", {
        "button": {"type": "string", "enum": list(KEYS)},
        "focus": {"type": "boolean", "default": False},
    }, ("button",)),
    tool("stop", "Ask the game window to close and wait briefly for the launcher's exit."),
]


def checked_int(value, low, high, name):
    if type(value) is not int or not low <= value <= high:
        raise ValueError(f"{name} must be an integer from {low} to {high}")
    return value


def xprop(*args):
    return subprocess.run(
        ["xprop", *args], capture_output=True, text=True, timeout=5, check=True,
    ).stdout


def window_ids(properties):
    return [int(match, 16) for match in WINDOW_ID.findall(properties)]


def read_tail(path, limit=65536):
    try:
        with path.open("rb") as stream:
            end = stream.seek(0, os.SEEK_END)
            stream.seek(max(0, end - limit))
            return stream.read().decode("utf-8", errors="replace").splitlines()
    except FileNotFoundError:
        return []


class ClientMessage(C.Structure):
    _fields_ = [
        ("type", C.c_int), ("serial", C.c_ulong), ("send_event", C.c_int),
        ("display", C.c_void_p), ("window", C.c_ulong), ("message_type", C.c_ulong),
        ("format", C.c_int), ("data", C.c_long * 5),
    ]


class XEvent(C.Union):
    _fields_ = [("message", ClientMessage), ("padding", C.c_long * 24)]


@contextmanager
def x_display():
    lib = C.CDLL("libX11.so.6")
    lib.XOpenDisplay.argtypes = [C.c_char_p]
    lib.XOpenDisplay.restype = C.c_void_p
    lib.XCloseDisplay.argtypes = [C.c_void_p]
    display = lib.XOpenDisplay(None)
    if not display:
        raise ValueError("Could not connect to X11 or XWayland")
    try:
        yield lib, display
    finally:
        lib.XCloseDisplay(display)


def window_message(lib, display, window, target, kind, value, mask=0):
    lib.XInternAtom.argtypes = [C.c_void_p, C.c_char_p, C.c_int]
    lib.XInternAtom.restype = C.c_ulong
    lib.XSendEvent.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_long, C.POINTER(XEvent)]
    atom = lib.XInternAtom(display, kind, 0)
    data = lib.XInternAtom(display, value, 0) if isinstance(value, bytes) else value
    event = XEvent(message=ClientMessage(
        33, 0, 1, display, window, atom, 32, (C.c_long * 5)(data),
    ))
    if not lib.XSendEvent(display, target, 0, mask, C.byref(event)):
        raise ValueError(f"The desktop rejected {kind.decode()}")


def close_window(window):
    with x_display() as (lib, display):
        window_message(lib, display, window, window, b"WM_PROTOCOLS", b"WM_DELETE_WINDOW")


def activate_window(lib, display, window):
    lib.XDefaultRootWindow.argtypes = [C.c_void_p]
    lib.XDefaultRootWindow.restype = C.c_ulong
    root = lib.XDefaultRootWindow(display)
    # The window manager handles EWMH activation requests sent to the root window.
    window_message(lib, display, window, root, b"_NET_ACTIVE_WINDOW", 2, (1 << 19) | (1 << 20))
    lib.XSync(display, 0)
    for _ in range(10):
        if window_ids(xprop("-root", "_NET_ACTIVE_WINDOW")) == [window]:
            return
        time.sleep(0.05)


def send_key(window, keysym, focus=False):
    with x_display() as (lib, display):
        lib.XKeysymToKeycode.argtypes = [C.c_void_p, C.c_ulong]
        lib.XKeysymToKeycode.restype = C.c_ubyte
        lib.XSync.argtypes = [C.c_void_p, C.c_int]
        xtest = C.CDLL("libXtst.so.6")
        xtest.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        key = lib.XKeysymToKeycode(display, keysym)
        if focus and window_ids(xprop("-root", "_NET_ACTIVE_WINDOW")) != [window]:
            activate_window(lib, display, window)
        if not key or window_ids(xprop("-root", "_NET_ACTIVE_WINDOW")) != [window]:
            raise ValueError("The GTA VI window must be focused to send input")
        if not xtest.XTestFakeKeyEvent(display, key, 1, 0):
            raise ValueError("The desktop rejected the game input")
        try:
            lib.XSync(display, 0)
            time.sleep(0.15)
        finally:
            xtest.XTestFakeKeyEvent(display, key, 0, 0)
            lib.XSync(display, 0)


class ScarlettSession:
    def __init__(self, args):
        self.args = args
        self.process = None
        self.run_dir = None
        self.run_id = None
        self.started = None

    def window(self):
        if self.process is None or self.process.poll() is not None:
            return None
        for window in window_ids(xprop("-root", "_NET_CLIENT_LIST")):
            try:
                props = xprop("-id", hex(window), "_NET_WM_NAME", "_NET_WM_PID")
                pid = re.search(r"_NET_WM_PID\(CARDINAL\) = (\d+)", props)
                if '"Americas"' not in props or pid is None:
                    continue
                proc = Path("/proc") / pid[1]
                env = (proc / "environ").read_bytes().split(b"\0")
                if f"TUXBOX_MCP_RUN_ID={self.run_id}".encode() in env:
                    return window
            except (OSError, subprocess.CalledProcessError):
                continue  # The window can disappear between enumeration and inspection.
        return None

    def launch(self, trace_interval=600, profile="packaged", trace_draws=False, primary_plane_only=False):
        if self.process is not None and self.process.poll() is None:
            raise ValueError("A GTA VI launch is already running")
        interval = checked_int(trace_interval, 0, 36000, "trace_interval")
        if type(trace_draws) is not bool:
            raise ValueError("trace_draws must be true or false")
        if type(primary_plane_only) is not bool:
            raise ValueError("primary_plane_only must be true or false")
        if profile not in ("packaged", "series-s"):
            raise ValueError("Unknown graphics profile")
        self.args.output.mkdir(parents=True, exist_ok=True)
        self.run_dir = Path(mkdtemp(prefix="run-", dir=self.args.output))
        self.run_id = uuid.uuid4().hex
        env = dict(os.environ, TUXBOX_MCP_RUN_ID=self.run_id, TUXBOX_TRACE_PRESENT=str(interval))
        if trace_draws:
            env.update(TUXBOX_TRACE_RASTER_DRAWS="1", TUXBOX_TRACE_RASTER_INPUTS="1",
                       TUXBOX_CAPTURE_DRAW_READBACK="1")
        if primary_plane_only:
            env["TUXBOX_PRESENT_PRIMARY_PLANE"] = "1"
        command = [
            sys.executable, str(ROOT / "tuxbox.py"), "run",
            "--offline", "--mangohud", "--prefix", str(self.args.prefix),
            "--log-file", str(self.run_dir / "game.log"),
            "--graphics-profile", profile,
        ]
        if self.args.proton:
            command.extend(["--proton", str(self.args.proton)])
        command.append(str(self.args.package))
        with (self.run_dir / "console.log").open("wb") as console:
            self.process = subprocess.Popen(command, cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                                            stdout=console, stderr=subprocess.STDOUT)
        self.started = time.monotonic()
        return {"pid": self.process.pid, "run_dir": str(self.run_dir), "profile": profile}

    def status(self, lines=20):
        count = checked_int(lines, 0, 100, "lines")
        if self.process is None:
            return {"state": "idle"}
        exit_code = self.process.poll()
        game_log = self.run_dir / "game.log"
        present = []
        if game_log.is_file():
            with game_log.open(encoding="utf-8", errors="replace") as log:
                present = list(deque((line.strip() for line in log if MILESTONES.search(line)), maxlen=12))
        csv = next(self.run_dir.glob("mangohud-*/*.csv"), None)
        try:
            window = self.window()
            window_error = None
        except (OSError, subprocess.SubprocessError) as error:
            window = None
            window_error = str(error)
        return {
            "state": "running" if exit_code is None else "exited",
            "exit_code": exit_code,
            "elapsed_seconds": round(time.monotonic() - self.started, 1) if exit_code is None else None,
            "window": hex(window) if window else None,
            "window_error": window_error,
            "run_dir": str(self.run_dir),
            "log_tail": read_tail(game_log if game_log.is_file() else self.run_dir / "console.log")[-count:] if count else [],
            "milestones": present,
            "mangohud_csv": str(csv) if csv else None,
            "mangohud_latest_samples": read_tail(csv, 4096)[-3:] if csv else [],
        }

    def game_window(self):
        window = self.window()
        if not window:
            raise ValueError("The current GTA VI launch has no visible X11 window")
        return window

    def screenshot(self):
        window = self.game_window()
        path = self.run_dir / f"window-{time.time_ns()}.png"
        subprocess.run(
            ["import", "-silent", "-window", hex(window), "-resize", "1920x1080>", str(path)],
            capture_output=True, check=True, timeout=10,
        )
        return [
            {"type": "text", "text": str(path)},
            {"type": "image", "mimeType": "image/png", "data": base64.b64encode(path.read_bytes()).decode("ascii")},
        ]

    def press(self, button, focus=False):
        if button not in KEYS:
            raise ValueError("Unknown controller button")
        if type(focus) is not bool:
            raise ValueError("focus must be true or false")
        send_key(self.game_window(), KEYS[button], focus)
        return {"pressed": button}

    def stop(self):
        if self.process is None or self.process.poll() is not None:
            return self.status(lines=0)
        window = self.game_window()
        if "WM_DELETE_WINDOW" not in xprop("-id", hex(window), "WM_PROTOCOLS"):
            raise ValueError("The GTA VI window does not advertise a close protocol")
        close_window(window)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        return self.status(lines=0)


def call_tool(session, params):
    name = params.get("name")
    if not any(name == item["name"] for item in TOOLS):
        return tool_error("Unknown tool")
    try:
        result = getattr(session, name)(**params.get("arguments", {}))
    except (OSError, TypeError, ValueError, subprocess.SubprocessError) as error:
        detail = getattr(error, "stderr", None)
        if isinstance(detail, bytes):
            detail = detail.decode("utf-8", errors="replace")
        return tool_error(detail.strip() if detail else str(error))
    content = result if isinstance(result, list) else [{"type": "text", "text": json.dumps(result)}]
    return {"content": content, "isError": False}


def tool_error(message):
    return {"content": [{"type": "text", "text": message}], "isError": True}


def response(session, message):
    if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
        return {"id": None, "error": {"code": -32600, "message": "Invalid request"}}
    if "id" not in message:
        return None
    request_id = message["id"]
    method = message.get("method")
    params = message.get("params", {})
    if not isinstance(params, dict):
        return {"id": request_id, "error": {"code": -32602, "message": "Invalid parameters"}}
    if method == "initialize":
        requested = params.get("protocolVersion")
        result = {
            "protocolVersion": requested if requested in ("2024-11-05", "2025-03-26", "2025-06-18") else "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "tuxbox-scarlett", "version": "1.0"},
        }
    elif method == "ping":
        result = {}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        result = call_tool(session, params)
    else:
        return {"id": request_id, "error": {"code": -32601, "message": "Unknown method"}}
    return {"id": request_id, "result": result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True, help="staged GTA VI package directory")
    parser.add_argument("--prefix", type=Path, required=True, help="Wine prefix dedicated to this test")
    parser.add_argument("--proton", type=Path, help="Proton distribution directory")
    parser.add_argument("--output", type=Path, default=ROOT / "build/scarlett-mcp", help="run artifact directory")
    args = parser.parse_args()
    for name in ("package", "prefix", "proton", "output"):
        value = getattr(args, name)
        if value is not None:
            setattr(args, name, value.expanduser().resolve())
    sys.path.insert(0, str(ROOT))
    from project_tuxbox.inspect import launch_target
    try:
        if not args.package.is_dir() or launch_target(args.package, None).name.lower() != "game_scarlett_master.exe":
            raise ValueError("--package must be a staged Project Scarlett package directory")
    except (OSError, ValueError) as error:
        parser.error(str(error))
    session = ScarlettSession(args)
    for line in sys.stdin:
        try:
            reply = response(session, json.loads(line))
        except json.JSONDecodeError:
            reply = {"id": None, "error": {"code": -32700, "message": "Invalid JSON"}}
        if reply is not None:
            print(json.dumps({"jsonrpc": "2.0", **reply}), flush=True)


if __name__ == "__main__":
    main()
