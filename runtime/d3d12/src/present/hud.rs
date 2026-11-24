//! RPCS3-style bottom-left shader compilation status overlay.
//! Drawn directly into an upload buffer with a built-in 5x7 bitmap font and
//! copied onto the swap-chain backbuffer while it is already in COPY_DEST.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Instant;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::core::Result;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 44;
// 640 * 4 = 2560 bytes, already a multiple of D3D12_TEXTURE_DATA_PITCH_ALIGNMENT (256).
const ROW_PITCH: u32 = WIDTH * 4;
const BUFFER_BYTES: usize = (ROW_PITCH * HEIGHT) as usize;
const HOLD_MS: u64 = 2500;

static EPOCH: OnceLock<Instant> = OnceLock::new();
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static COMPILED: AtomicUsize = AtomicUsize::new(0);
static CACHED: AtomicUsize = AtomicUsize::new(0);
static LAST_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    let start = EPOCH.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64 + 1
}

pub(crate) fn shader_cached() {
    CACHED.fetch_add(1, Ordering::Relaxed);
    LAST_MS.store(now_ms(), Ordering::Relaxed);
}

pub(crate) struct CompileGuard {
    succeeded: bool,
}

impl CompileGuard {
    pub(crate) fn start() -> Self {
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        LAST_MS.store(now_ms(), Ordering::Relaxed);
        Self { succeeded: false }
    }

    pub(crate) fn finish(mut self, succeeded: bool) {
        self.succeeded = succeeded;
    }
}

impl Drop for CompileGuard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
        if self.succeeded {
            COMPILED.fetch_add(1, Ordering::Relaxed);
        }
        LAST_MS.store(now_ms(), Ordering::Relaxed);
    }
}

pub(super) struct Overlay {
    upload: ID3D12Resource,
    mapped: *mut u32,
    format: DXGI_FORMAT,
    frame: u32,
}

// SAFETY: Overlay is owned by SwapChain under Presentation's Mutex.
unsafe impl Send for Overlay {}

impl Overlay {
    pub(super) fn new(device: &ID3D12Device, format: DXGI_FORMAT) -> Result<Self> {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_UPLOAD,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: BUFFER_BYTES as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut upload: Option<ID3D12Resource> = None;
        let mut ptr = std::ptr::null_mut();
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut upload,
            )?;
            let upload = upload.ok_or(windows::Win32::Foundation::E_FAIL)?;
            let empty = D3D12_RANGE { Begin: 0, End: 0 };
            upload.Map(0, Some(&empty), Some(&mut ptr))?;
            Ok(Self {
                upload,
                mapped: ptr.cast::<u32>(),
                format,
                frame: 0,
            })
        }
    }

    /// Copy the shader compilation HUD onto the bottom-left corner of `buffer`
    /// (which must currently be in `D3D12_RESOURCE_STATE_COPY_DEST`).
    pub(super) unsafe fn record(
        &mut self,
        list: &ID3D12GraphicsCommandList,
        buffer: &ID3D12Resource,
        target_w: u32,
        target_h: u32,
    ) {
        if target_w < WIDTH + 32 || target_h < HEIGHT + 32 {
            return;
        }
        let active = ACTIVE.load(Ordering::Relaxed);
        let compiled = COMPILED.load(Ordering::Relaxed);
        let cached = CACHED.load(Ordering::Relaxed);
        let last = LAST_MS.load(Ordering::Relaxed);
        if last == 0 || (active == 0 && now_ms().saturating_sub(last) > HOLD_MS) {
            return;
        }
        self.frame = self.frame.wrapping_add(1);
        let text = if active > 0 {
            let spinner = ['|', '/', '-', '\\'][(self.frame / 6) as usize % 4];
            if cached > 0 {
                format!("{spinner} COMPILING SHADERS: {compiled}  ({active} ACTIVE, {cached} CACHED)")
            } else {
                format!("{spinner} COMPILING SHADERS: {compiled}  ({active} ACTIVE)")
            }
        } else if compiled > 0 {
            format!("SHADERS COMPILED: {compiled}  ({cached} CACHED)")
        } else {
            format!("LOADING SHADER CACHE: {cached}")
        };

        let pixels = unsafe { std::slice::from_raw_parts_mut(self.mapped, (WIDTH * HEIGHT) as usize) };
        let bg = pack(self.format, 18, 22, 30);
        let border = pack(self.format, 60, 74, 96);
        let accent = pack(self.format, 56, 189, 248);
        let fg = pack(self.format, 240, 246, 252);
        let bar_bg = pack(self.format, 32, 40, 54);

        pixels.fill(bg);
        for x in 0..WIDTH as usize {
            pixels[x] = border;
            pixels[(HEIGHT as usize - 1) * WIDTH as usize + x] = border;
        }
        for y in 0..HEIGHT as usize {
            let row = y * WIDTH as usize;
            pixels[row] = border;
            pixels[row + WIDTH as usize - 1] = border;
            for x in 1..6 {
                pixels[row + x] = accent;
            }
        }
        // Subtle animated activity bar along the bottom while shaders are actively compiling.
        if active > 0 {
            let bar_w = WIDTH as usize - 20;
            let bar_y = HEIGHT as usize - 7;
            let sweep = ((self.frame as usize * 8) % (bar_w + 100)).saturating_sub(50);
            for dy in 0..3 {
                let row = (bar_y + dy) * WIDTH as usize;
                for x in 0..bar_w {
                    let lit = x >= sweep && x < sweep + 96;
                    pixels[row + 12 + x] = if lit { accent } else { bar_bg };
                }
            }
        }

        // Draw crisp 2x scaled 5x7 text.
        draw_text(pixels, 16, 11, &text, fg);

        let dst = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::ManuallyDrop::new(Some(buffer.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };
        let src = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::ManuallyDrop::new(Some(self.upload.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: self.format,
                        Width: WIDTH,
                        Height: HEIGHT,
                        Depth: 1,
                        RowPitch: ROW_PITCH,
                    },
                },
            },
        };
        let dst_x = 24;
        let dst_y = target_h - HEIGHT - 24;
        unsafe {
            list.CopyTextureRegion(&dst, dst_x, dst_y, 0, &src, None);
            let mut dst = dst;
            let mut src = src;
            std::mem::ManuallyDrop::drop(&mut dst.pResource);
            std::mem::ManuallyDrop::drop(&mut src.pResource);
        }
    }
}

fn pack(format: DXGI_FORMAT, r: u8, g: u8, b: u8) -> u32 {
    if format == DXGI_FORMAT_R10G10B10A2_UNORM {
        let r10 = (u32::from(r) * 1023 + 127) / 255;
        let g10 = (u32::from(g) * 1023 + 127) / 255;
        let b10 = (u32::from(b) * 1023 + 127) / 255;
        r10 | (g10 << 10) | (b10 << 20) | (3 << 30)
    } else {
        debug_assert_eq!(format, DXGI_FORMAT_R8G8B8A8_UNORM);
        u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16) | (0xff << 24)
    }
}

fn draw_text(pixels: &mut [u32], start_x: usize, start_y: usize, text: &str, color: u32) {
    let mut cx = start_x;
    for ch in text.bytes() {
        if cx + 12 >= WIDTH as usize {
            break;
        }
        let rows = glyph(ch);
        for (ry, &bits) in rows.iter().enumerate() {
            for rx in 0..5 {
                if (bits >> (4 - rx)) & 1 != 0 {
                    let px = cx + rx * 2;
                    let py = start_y + ry * 2;
                    for dy in 0..2 {
                        let base = (py + dy) * WIDTH as usize + px;
                        pixels[base] = color;
                        pixels[base + 1] = color;
                    }
                }
            }
        }
        cx += 12;
    }
}

fn glyph(ch: u8) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        b'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        b'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        b'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        b'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        b'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        b'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110],
        b'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        b'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        b'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        b'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        b'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        b'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        b'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        b'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        b'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        b'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        b'0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        b'1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        b'3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        b'4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        b'5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        b'6' => [0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110],
        b'7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        b'8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        b'9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        b':' => [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000],
        b',' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00100, 0b00100, 0b01000],
        b'(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        b')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        b'|' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        b'/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        b'\\' => [0b10000, 0b01000, 0b01000, 0b00100, 0b00010, 0b00010, 0b00001],
        b'-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        _ => [0; 7],
    }
}
