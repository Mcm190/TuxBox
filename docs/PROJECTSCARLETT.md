# Set up ProjectScarlett (GTA VI) on Linux

This guide stages GTA VI 1.0.2.1 (`ProjectScarlett`) with TuxBox's providers and starts it on Valve's stock Proton Experimental. The packaged profile reached player selection at 3840×2160 on GeForce and completed first-run setup on Radeon. The recorded Radeon Series S runs presented black frames at 1920×1080. [Title progress](PROGRESS.md) · [GeForce run record](../evidence/projectscarlett-startup.json)

You supply the game package. TuxBox doesn't include any game files. The recorded runs use package `RockstarGamesInc.28872443D20A7` 1.0.2.1, whose `game_scarlett_master.exe` has SHA-256 `e95020b2fa4a721db3df59fdecf75621730ac559d8835252f6c3b658d5a08993`. Other builds may need different providers or shader requests.

## Requirements

- The project's Linux build tools: Python 3.10 or newer, Rust with the `x86_64-pc-windows-msvc` target, `clang-cl`, `lld-link` and the xwin Windows SDK. [Build requirements](../examples/lanes/README.md#build-requirements)
- Proton Experimental from Steam and `umu-run`.
- Microsoft's Linux DXC release with `dxc` and `dxv`, for shader preparation and runtime translation. Arch's `directx-shader-compiler` 1.10.2605.37 fails on a minimal compute shader. Use Microsoft's release instead. [DirectX Shader Compiler releases](https://github.com/microsoft/DirectXShaderCompiler/releases)
- The Windows x64 `dxcompiler.dll` and `dxil.dll` from Microsoft's DXC release. The D3D12 provider loads them in Proton to compile geometry shaders for Xbox screen rectangles. [Verified DXC release](https://github.com/microsoft/DirectXShaderCompiler/releases/tag/v1.8.2505.1)
- A C compiler and Vulkan headers. The launcher builds TuxBox's constant-buffer Vulkan layer on first use.

Run `python tuxbox.py inspect /path/to/package` to confirm the package. It should report one executable entry, `GTAO  Scarlett  game_scarlett_master.exe  present`.

## Build the providers

```sh
python -m pip install -r requirements.txt
python build.py
```

To build only what ProjectScarlett loads, run `python build.py <target>` for each of `xmem`, `d3d12`, `xg`, `xapu`, `acphal`, `xcrdapi`, `gameinput` and `xgameruntime`. Some Linux distributions currently need the build workarounds described in [issue #4](https://github.com/brcly/TuxBox/issues/4).

## Stage a working copy

Keep the original package unchanged and launch a copy. The package doesn't contain any of the provider DLL names, so TuxBox's copies are added beside the game files rather than replacing them.

```sh
cp -a --reflink=auto /path/to/GTA6.Xbox.v1.0.2.1 /path/to/scarlett-run
cp build/xmem-smoke/{xmem,tuxbox_xhttp}.dll \
   build/d3d12-smoke/d3d12_xs.dll \
   build/xg-provider/xg_xs.dll \
   build/xapu/{xapu,chad}.dll \
   build/acphal/AcpHal.dll \
   build/xcrdapi/xcrdapi.dll \
   build/gameinput/GameInput.dll \
   build/xgameruntime/xgameruntime.dll \
   /path/to/scarlett-run/
```

On a copy-on-write filesystem such as Btrfs or XFS, `--reflink=auto` copies the package almost instantly without using more disk space. If you use a hard-link copy (`cp -al`) to save space, never write into the linked game files. Writing to one also changes the original package.

| Provider | Build output | Loaded by |
| --- | --- | --- |
| `xmem.dll` | `build/xmem-smoke/` | the executable, `Bink2Scarlett.dll`, `XboxBvhBuilder.dll`, `amd_fidelityfx_upscaler_xbox.dll` |
| `tuxbox_xhttp.dll` | `build/xmem-smoke/` | `xmem.dll`, which forwards `XMemSetWinHttpHooks` to it |
| `d3d12_xs.dll` | `build/d3d12-smoke/` | the executable, `XboxBvhBuilder.dll`, `amd_fidelityfx_upscaler_xbox.dll` |
| `xg_xs.dll` | `build/xg-provider/` | the executable |
| `xapu.dll`, `chad.dll` | `build/xapu/` | the executable |
| `AcpHal.dll` | `build/acphal/` | the executable |
| `xcrdapi.dll` | `build/xcrdapi/` | the executable |
| `GameInput.dll` | `build/gameinput/` | the executable |
| `xgameruntime.dll` | `build/xgameruntime/` | the executable, at startup by name |

Copy the providers again after each rebuild. The launcher enables the constant-buffer layer only when the staged `d3d12_xs.dll` is byte-identical to `build/d3d12-smoke/d3d12_xs.dll`.

Copy `bin/x64/dxcompiler.dll` and `bin/x64/dxil.dll` from the Windows DXC archive beside `game_scarlett_master.exe`. The Linux shader compiler and validator stay on the host.

## Launch offline

The game installs a Breakpad crash handler that uploads minidumps to an online crash service. Run it without network access so that crashes during testing are not uploaded. On Linux, start the launcher in a new network namespace:

```sh
UMU_RUNTIME_UPDATE=0 unshare --user --map-current-user --net -- \
    python tuxbox.py run --log-file build/scarlett.log /path/to/scarlett-run
```

Inside the namespace the only network device is a loopback interface that is down. `UMU_RUNTIME_UPDATE=0` stops umu-run from trying to update the Steam Runtime. Offline launches need unprivileged user namespaces.

Add `--mangohud` before the package path to record frame times. The prefix defaults to `~/.local/share/project-tuxbox/prefixes/Proton - Experimental`. Pass `--prefix` to use another one.

## Test with an MCP client

Configure a local stdio MCP server to run the command below. Start it from the desktop session with the same Python environment, `PATH` and shader tools as the launcher. The window tools use X11 or XWayland, `xprop`, ImageMagick's `import` and libXtst.

```sh
python tools/scarlett_mcp.py --package /path/to/scarlett-run --prefix build/scarlett-mcp-prefix
```

The server uses its own launch session. `launch` starts the game offline with MangoHud. By default it reads back the presented source every 600 frames. Set `trace_interval` to zero for a run without GPU readback or enable `trace_draws` to sample individual raster draws. `status` shows the log tail, presentation milestones and recent frame samples. `screenshot` saves the game window and returns it to the client. `press` sends a controller button through the keyboard mapping while the game has focus. Set `focus` to request window focus before sending the button. `stop` requests window close. Logs and captures go under `build/scarlett-mcp/` by default.

The `primary_plane_only` diagnostic presents plane zero when the title submits two planes. With GPU readback enabled, it also samples a compatible secondary plane and logs its RGB and alpha values. For a launch outside MCP, set `TUXBOX_PRESENT_PRIMARY_PLANE=1` before running TuxBox.

Set `TUXBOX_TRACE_GAMEINPUT=1` to log the first gamepad read and button changes. Set `TUXBOX_TRACE_HOST_IMPORTS=1` to record the guest VA range, Windows memory type, resource flags and D3D12 result for each sized buffer heap lookup.

## Shader preparation

The game records requested shaders in the active prefix, and a worker compiles them while it runs. Pass `--prepare-only` before the package path to compile the available requests into the base prefix's cache before launching. Series S mode starts from a temporary copy of that prefix. [Preparation results](../evidence/projectscarlett-shader-preparation.json)

## What to expect

The game opens its Americas window, loads its archives and presents 3840×2160 frames. The player selection prompt shows Account Picker, Screen Narration and TuxBox. The recorded GeForce run stopped at a title breakpoint 97 seconds after launch. On Radeon, Enter maps to controller A and advanced through the opening sequence and first-run setup. Confirming the legal terms led to a black loading screen. Gameplay remains unverified. [Current priorities](PROGRESS.md#current-priorities)

In a new prefix, the game stops at a breakpoint during System Init because it reads its package through the `G:` drive, as on the console, and the launcher doesn't map that drive yet. A fix is in review. [Pull request #6](https://github.com/brcly/TuxBox/pull/6)

When you report a run, include the package version, executable hash, GPU and driver, Proton build, and the log printed in the first line of the launcher's output.

## Radeon run milestones on November 24, 2026

These runs used the matching 1.0.2.1 executable, an RX 9070 XT with Mesa RADV 26.2.7, Heroic's UMU 1.4.4 and Proton Experimental `experimental-11.0-20261112`. The staged package used read-only links to the large archives, and all launches ran offline. The packaged profile submits 3840×2160 frames internally regardless of the monitor's resolution.

| Run | Verified milestone |
| --- | --- |
| Cold launch, `1870fd9` | Opened the Americas window and presented its first frame about eight seconds after launch. The title opened its main RPF archives through `G:` and signed in the local user. The window stayed black through the observed startup. |
| Warm launch through MCP, `221f9ec` | Verified launch, status, window capture and close. GPU readback sampled the first presented frame and every 600th frame through sample 11. Each sample was black. After the first sample, each interval recorded 600 clears, 600 draws overall and zero draws or copies to the display targets. The launch exited with code 0 after the MCP close request at 137 seconds. |
| Warm launch with Windows DXC | TuxBox created the screen-rectangle pipeline, and the earlier slot 86 `0x8007007e` error cleared. A stable interval of 300 frames recorded 300 display draws and 900 draws overall. All 32 samples were black through the clean exit at 183 seconds. A confirm input produced no observable in-game response. |
| Series S profile | The title submitted two display planes. The provider returned `E_NOTIMPL` and MangoHud recorded zero presents. The window closed on request, and the still-running title process was stopped at 149 seconds. |
| Series S with primary-plane presentation | The provider accepted the full 1920×1080 rectangle of the primary plane and presented it through the desktop swap chain. All 35 presentation samples through the clean exit at 193 seconds were black. Four per-draw samples from a 3840×2160 display resource were also black. |
| Series S with both planes sampled | Plane zero was RGBA8 at 1920×1080. Plane one was RGBA8 at 3840×2160. Both requested full-frame mapping. All 27 readback pairs showed black RGB and zero mean alpha. The run closed cleanly at 150 seconds. |
| Series S with no default local user | The title received `E_GAMEUSER_NO_DEFAULT_USER` from `XUserAddResult`. Neither plane showed visible content before or after a controller Y input. The run closed cleanly at 179 seconds. [GDK sign-in behavior](https://learn.microsoft.com/en-us/gaming/gdk/docs/reference/system/xuser/functions/xuseraddasync) |
| Series S accessibility startup | After TuxBox served `XAccessibility`, the title created a local speech handle and enumerated zero installed voices. Local sign-in returned the user handle. Both display planes stayed black. The Proton runtime fixture verified the caption, contrast, voice enumeration and speech handle calls. [xcl interface slots](https://github.com/ivall/xcl/blob/main/crates/xcl/src/hle/xgameruntime/gdk_table.rs) · [GDK text-to-speech](https://learn.microsoft.com/en-us/gaming/gdk/docs/features/console/text-to-speech/text-to-speech) |
| Series S command ordering | A temporary diagnostic accepted a GPU memory wait after a clear and logged the subsequent render command on the same list. The list closed with `E_NOTIMPL` under the diagnostic. Both sampled display planes stayed black through the clean exit at 161 seconds. |
| Package splash preview | The dump's `xbs_splash.png` is 1920×1080 and opened in a separate desktop window. Game frame verification still requires visible pixels from the title's display planes. |
| Historical D3D12 provider on Radeon | Built `d3d12_xs.dll` from `c217aa4`, the revision that reported player selection on GeForce, and used the current launcher and installed Proton build. The packaged profile read back black 3840×2160 frames through the clean exit at 106 seconds. |
| Series S wait and cache-barrier fixes | The trace identified an enhanced barrier and an RTV clear before the rejected GPU wait. TuxBox submitted the list in order and the wait completed. A separate `E_INVALIDARG` came from Device11 slot 131, which the title calls with cache and engine masks and a VA range at RVA `0x1560994`. The corrected mapping uses the enhanced global cache barrier. |
| Packaged profile with command-buffer import reuse, `1172b73` | Player selection appeared at about 135 seconds. GameInput recorded X, Y and A. Y initiated an account-selection request. A advanced to the six-star screen. Sampled player-selection frame rates were 1.8 FPS before the cache and 7.4 FPS after it, with different diagnostic settings in the two runs. |
| Packaged profile with A pressed first | The six-star screen was followed by the opening graphic. A GPU readback measured mean RGB `(133, 117, 53)` at 3840×2160, and the window capture showed the full-colour opening artwork. The run closed cleanly at 173.5 seconds. [Opening screenshot](../evidence/projectscarlett-radeon-opening.webp) |
| Packaged profile through first-run setup | The opening sequence led into navigation, UI text size, subtitles and brightness settings. The title accepted the default values, displayed the legal notices and terms, and started loading after confirmation. The next sampled display frames were black while shader compilation continued. |
| First world-load attempt | The title continued compiling shaders after setup. A 188-batch `CopyPageMappingsBatchX` call returned `E_NOTIMPL` with flags `0`. The provider later stopped when the title requested a buffer SRV over a still-reserved page at `0xb94d70000`. The process exited with code 2. |
| World-load retry with mapping diagnostics | A 189-batch update tried to unmap the live 64 KiB buffer import at `0xbbf230000`. Earlier in the run, RADV rejected the host import for that `MEM_MAPPED` page and vkd3d-proton returned success through its fallback. TuxBox rejected the mapping batch, then stopped when the title requested a buffer SRV over a different still-reserved page at `0xbbe5f0000`. The process again exited with code 2. |

During the initial warm run, the title's slot 86 pipeline stream returned `0x8007007e`. TuxBox's screen-rectangle path loads Windows `dxcompiler.dll` inside the game process. Staging the Windows x64 compiler and validator from DXC v1.8.2505.1 restored that pipeline.

An enhanced barrier and RTV clear can now precede a GPU memory wait while preserving the title's list order. The rejected startup wait went through after this change. The wait fix alone did not restore visible output. The warmed Radeon prefix had 1,726 cached shader translations and 1,411 unsupported requests when checked with `--prepare-only`. The renderer can substitute resource-free shaders for missing compute pipelines, so a successful boot or present does not establish shader coverage.

The Radeon trace correlated 130 host-memory import warnings with `MEM_MAPPED` ranges. Of those, 128 requested unordered-access buffers and two requested buffers with no resource flags. The start page of a sampled 50 MiB UAV came from a single-page `XMemMapPhysicalPages` call. All 130 heap lookups returned success through vkd3d-proton's host-visible fallback. A local GPU probe read zero from a section-backed fallback buffer after CPU writes, while private placeholder memory preserved those writes. The affected UAVs need backing that keeps CPU mappings and GPU writes coherent. [vkd3d-proton host-memory fallback](https://github.com/HansKristian-Work/vkd3d-proton/blob/master/libs/vkd3d/memory.c#L1110-L1134)

VIctory reports working first-boot menus and intro video on its Windows backend. An xcl report for this package reached first-run setup and later hit a title fault. Both projects select the primary display plane on the host. In TuxBox, 35 shader programs that reported sampler errors without a root signature also had requests with their pipeline root attached. The rooted variants progressed past that sampler error, with several then refused on divergent wave reductions. [VIctory status](https://github.com/Marvisak/VIctory) · [VIctory presentation](https://github.com/Marvisak/VIctory/blob/main/crates/victory-gpu/src/queue.rs) · [xcl first-run report](https://github.com/ivall/xcl/issues/846) · [xcl presentation](https://github.com/ivall/xcl/blob/main/crates/xcl/src/hle/d3d12x/present.rs)
