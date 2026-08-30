use parking_lot::RwLock;
use rayon::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::watch;

/// Standard RFB 3.8 Pixel Format definition
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian_flag: u8,
    pub true_colour_flag: u8,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// Standard 32-bit RGBA (8-8-8-8, Little Endian: B, G, R, A in memory)
    pub fn rgba32() -> Self {
        Self {
            bits_per_pixel: 32,
            depth: 24,
            big_endian_flag: 0,
            true_colour_flag: 1,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 0,
            green_shift: 8,
            blue_shift: 16,
        }
    }

    /// Standard 32-bit BGRA (8-8-8-8, Little Endian: R, G, B, A in memory)
    pub fn bgra32() -> Self {
        Self {
            bits_per_pixel: 32,
            depth: 24,
            big_endian_flag: 0,
            true_colour_flag: 1,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        }
    }

    /// Standard 24-bit RGB (3 bytes per pixel: R, G, B)
    pub fn rgb24() -> Self {
        Self {
            bits_per_pixel: 24,
            depth: 24,
            big_endian_flag: 0,
            true_colour_flag: 1,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        }
    }

    /// Serialize pixel format into 16 bytes for RFB protocol
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0] = self.bits_per_pixel;
        buf[1] = self.depth;
        buf[2] = self.big_endian_flag;
        buf[3] = self.true_colour_flag;
        buf[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        buf[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        buf[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        buf[10] = self.red_shift;
        buf[11] = self.green_shift;
        buf[12] = self.blue_shift;
        // 13, 14, 15 are 3 padding bytes
        buf
    }

    /// Parse 16 bytes from RFB protocol into PixelFormat
    pub fn from_bytes(buf: &[u8; 16]) -> Self {
        Self {
            bits_per_pixel: buf[0],
            depth: buf[1],
            big_endian_flag: buf[2],
            true_colour_flag: buf[3],
            red_max: u16::from_be_bytes([buf[4], buf[5]]),
            green_max: u16::from_be_bytes([buf[6], buf[7]]),
            blue_max: u16::from_be_bytes([buf[8], buf[9]]),
            red_shift: buf[10],
            green_shift: buf[11],
            blue_shift: buf[12],
        }
    }
}

/// A 2D rectangular area on the screen
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }
}

/// Size of individual damage tracking tile (32x32)
pub const TILE_SIZE: usize = 32;

/// Fast 64-bit FNV-1a inspired hash for SIMD/fast differencing of tile pixel data
#[inline(always)]
fn hash_tile_data(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let (chunks, remainder) = data.as_chunks::<8>();

    for chunk in chunks {
        let val = u64::from_le_bytes(*chunk);
        hash ^= val;
        hash = hash.wrapping_mul(0x100000001b3);
    }

    for &byte in remainder {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Tile-based Framebuffer with SIMD/64-bit damage tracking and zero-copy access
pub struct TileFramebuffer {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
    /// 32-bit pixel data (BGRA32 by default)
    pub pixels: Vec<u8>,
    /// Number of tiles in X and Y
    pub tiles_x: usize,
    pub tiles_y: usize,
    /// Hashes of tiles from the previous damage check
    tile_hashes: Vec<u64>,
    /// Damage flag per tile
    dirty_tiles: Vec<bool>,
    /// Global frame revision counter
    pub frame_version: u64,
}

impl TileFramebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let stride = (width as usize) * 4;
        let total_bytes = stride * (height as usize);
        let tiles_x = (width as usize).div_ceil(TILE_SIZE);
        let tiles_y = (height as usize).div_ceil(TILE_SIZE);
        let total_tiles = tiles_x * tiles_y;

        Self {
            width,
            height,
            stride,
            format: PixelFormat::bgra32(),
            pixels: vec![0u8; total_bytes],
            tiles_x,
            tiles_y,
            tile_hashes: vec![0u64; total_tiles],
            dirty_tiles: vec![true; total_tiles],
            frame_version: 1,
        }
    }

    /// Resize framebuffer and reinitialize tiles
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.stride = (width as usize) * 4;
        let total_bytes = self.stride * (height as usize);
        self.tiles_x = (width as usize).div_ceil(TILE_SIZE);
        self.tiles_y = (height as usize).div_ceil(TILE_SIZE);
        let total_tiles = self.tiles_x * self.tiles_y;

        self.pixels.resize(total_bytes, 0);
        self.tile_hashes = vec![0u64; total_tiles];
        self.dirty_tiles = vec![true; total_tiles];
        self.frame_version = self.frame_version.wrapping_add(1);
    }

    /// Direct zero-copy slice of raw pixels
    pub fn raw_slice(&self) -> &[u8] {
        &self.pixels
    }

    /// Copy a rectangular image chunk into the framebuffer and mark corresponding tiles dirty
    pub fn update_rect(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        src_data: &[u8],
        src_stride: usize,
    ) {
        if x >= self.width || y >= self.height || w == 0 || h == 0 {
            return;
        }
        let clamp_w = (w).min(self.width - x) as usize;
        let clamp_h = (h).min(self.height - y) as usize;
        let bytes_per_pixel = 4;
        let row_bytes = clamp_w * bytes_per_pixel;

        for row in 0..clamp_h {
            let dst_y = (y as usize) + row;
            let dst_start = dst_y * self.stride + (x as usize) * bytes_per_pixel;
            let dst_end = dst_start + row_bytes;

            let src_start = row * src_stride;
            let src_end = src_start + row_bytes;

            if src_end <= src_data.len() && dst_end <= self.pixels.len() {
                self.pixels[dst_start..dst_end].copy_from_slice(&src_data[src_start..src_end]);
            }
        }

        // Mark affected tiles as dirty
        let tile_start_x = (x as usize) / TILE_SIZE;
        let tile_end_x = ((x as usize + clamp_w - 1) / TILE_SIZE).min(self.tiles_x - 1);
        let tile_start_y = (y as usize) / TILE_SIZE;
        let tile_end_y = ((y as usize + clamp_h - 1) / TILE_SIZE).min(self.tiles_y - 1);

        for ty in tile_start_y..=tile_end_y {
            for tx in tile_start_x..=tile_end_x {
                let idx = ty * self.tiles_x + tx;
                self.dirty_tiles[idx] = true;
            }
        }
        self.frame_version = self.frame_version.wrapping_add(1);
    }

    /// Mark all tiles as dirty (full screen refresh)
    pub fn mark_all_damaged(&mut self) {
        for dirty in self.dirty_tiles.iter_mut() {
            *dirty = true;
        }
        self.frame_version = self.frame_version.wrapping_add(1);
    }

    /// Detect changed tiles using fast 64-bit differencing, update hashes, and return damaged rectangles
    pub fn detect_damage_tiles(&mut self) -> Vec<Rect> {
        let mut damaged_rects = Vec::new();
        let bytes_per_pixel = 4;

        for ty in 0..self.tiles_y {
            let tile_y = ty * TILE_SIZE;
            let tile_h = TILE_SIZE.min(self.height as usize - tile_y);

            for tx in 0..self.tiles_x {
                let tile_idx = ty * self.tiles_x + tx;
                let tile_x = tx * TILE_SIZE;
                let tile_w = TILE_SIZE.min(self.width as usize - tile_x);

                // Compute tile hash by sampling rows
                let mut tile_hash: u64 = 0xcbf29ce484222325;
                for row in 0..tile_h {
                    let cur_y = tile_y + row;
                    let row_start = cur_y * self.stride + tile_x * bytes_per_pixel;
                    let row_end = row_start + tile_w * bytes_per_pixel;
                    if row_end <= self.pixels.len() {
                        let row_hash = hash_tile_data(&self.pixels[row_start..row_end]);
                        tile_hash ^= row_hash.wrapping_add((row as u64) << 32);
                        tile_hash = tile_hash.wrapping_mul(0x100000001b3);
                    }
                }

                if self.dirty_tiles[tile_idx] || tile_hash != self.tile_hashes[tile_idx] {
                    self.tile_hashes[tile_idx] = tile_hash;
                    self.dirty_tiles[tile_idx] = false;

                    damaged_rects.push(Rect::new(
                        tile_x as u16,
                        tile_y as u16,
                        tile_w as u16,
                        tile_h as u16,
                    ));
                }
            }
        }

        damaged_rects
    }

    /// Zero-copy / pre-allocated extract into target destination buffer with Rayon scanline parallelization
    pub fn extract_rect_bytes_into(
        &self,
        rect: &Rect,
        target_format: &PixelFormat,
        out: &mut [u8],
    ) {
        let rx = rect.x as usize;
        let ry = rect.y as usize;
        let rw = rect.width as usize;
        let rh = rect.height as usize;
        let bpp = (target_format.bits_per_pixel / 8) as usize;
        let dst_stride = rw * bpp;

        let is_bgra = target_format.bits_per_pixel == 32
            && target_format.red_shift == 16
            && target_format.blue_shift == 0;
        let is_rgba = target_format.bits_per_pixel == 32
            && target_format.red_shift == 0
            && target_format.blue_shift == 16;
        let is_rgb24 = target_format.bits_per_pixel == 24
            && target_format.red_shift == 0
            && target_format.blue_shift == 16;
        let is_bgr24 = target_format.bits_per_pixel == 24
            && target_format.red_shift == 16
            && target_format.blue_shift == 0;

        // Use parallel rayon scanlines if area is large enough (>= 16 rows)
        if rh >= 16 {
            out.par_chunks_exact_mut(dst_stride)
                .take(rh)
                .enumerate()
                .for_each(|(row, dst_row)| {
                    let src_y = ry + row;
                    let src_row_start = src_y * self.stride + rx * 4;
                    let src_row = &self.pixels[src_row_start..src_row_start + rw * 4];

                    if is_bgra {
                        dst_row[..rw * 4].copy_from_slice(&src_row[..rw * 4]);
                    } else if is_rgba {
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * 4;
                            let b = src_row[s];
                            let g = src_row[s + 1];
                            let r = src_row[s + 2];
                            let a = src_row[s + 3];
                            dst_row[d] = r;
                            dst_row[d + 1] = g;
                            dst_row[d + 2] = b;
                            dst_row[d + 3] = a;
                        }
                    } else if is_rgb24 {
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * 3;
                            let b = src_row[s];
                            let g = src_row[s + 1];
                            let r = src_row[s + 2];
                            dst_row[d] = r;
                            dst_row[d + 1] = g;
                            dst_row[d + 2] = b;
                        }
                    } else if is_bgr24 {
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * 3;
                            dst_row[d] = src_row[s];
                            dst_row[d + 1] = src_row[s + 1];
                            dst_row[d + 2] = src_row[s + 2];
                        }
                    } else {
                        // Generic fallback
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * bpp;
                            let b = src_row[s];
                            let g = src_row[s + 1];
                            let r = src_row[s + 2];
                            let a = src_row[s + 3];
                            if bpp == 4 {
                                dst_row[d] = r;
                                dst_row[d + 1] = g;
                                dst_row[d + 2] = b;
                                dst_row[d + 3] = a;
                            } else if bpp == 3 {
                                dst_row[d] = r;
                                dst_row[d + 1] = g;
                                dst_row[d + 2] = b;
                            }
                        }
                    }
                });
        } else {
            // Small tile sequential fast path
            for row in 0..rh {
                let src_y = ry + row;
                let src_row_start = src_y * self.stride + rx * 4;
                let dst_row_start = row * dst_stride;

                if src_row_start + rw * 4 <= self.pixels.len()
                    && dst_row_start + dst_stride <= out.len()
                {
                    let src_row = &self.pixels[src_row_start..src_row_start + rw * 4];
                    let dst_row = &mut out[dst_row_start..dst_row_start + dst_stride];

                    if is_bgra {
                        dst_row.copy_from_slice(src_row);
                    } else if is_rgba {
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * 4;
                            dst_row[d] = src_row[s + 2]; // R
                            dst_row[d + 1] = src_row[s + 1]; // G
                            dst_row[d + 2] = src_row[s]; // B
                            dst_row[d + 3] = src_row[s + 3]; // A
                        }
                    } else if is_rgb24 {
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * 3;
                            dst_row[d] = src_row[s + 2]; // R
                            dst_row[d + 1] = src_row[s + 1]; // G
                            dst_row[d + 2] = src_row[s]; // B
                        }
                    } else if is_bgr24 {
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * 3;
                            dst_row[d] = src_row[s]; // B
                            dst_row[d + 1] = src_row[s + 1]; // G
                            dst_row[d + 2] = src_row[s + 2]; // R
                        }
                    } else {
                        for col in 0..rw {
                            let s = col * 4;
                            let d = col * bpp;
                            let b = src_row[s];
                            let g = src_row[s + 1];
                            let r = src_row[s + 2];
                            let a = src_row[s + 3];
                            if bpp == 4 {
                                dst_row[d] = r;
                                dst_row[d + 1] = g;
                                dst_row[d + 2] = b;
                                dst_row[d + 3] = a;
                            } else if bpp == 3 {
                                dst_row[d] = r;
                                dst_row[d + 1] = g;
                                dst_row[d + 2] = b;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Extract a sub-rectangle pixel buffer into target format
    pub fn extract_rect_bytes(&self, rect: &Rect, target_format: &PixelFormat) -> Vec<u8> {
        let rw = rect.width as usize;
        let rh = rect.height as usize;
        let bpp = (target_format.bits_per_pixel / 8) as usize;
        let mut out = vec![0u8; rw * rh * bpp];
        self.extract_rect_bytes_into(rect, target_format, &mut out);
        out
    }
}

/// Thread-safe shared handle to Framebuffer with change notification
#[derive(Clone)]
pub struct SharedFramebuffer {
    pub inner: Arc<RwLock<TileFramebuffer>>,
    pub notify_tx: watch::Sender<u64>,
    pub notify_rx: watch::Receiver<u64>,
    frame_counter: Arc<AtomicU64>,
}

impl SharedFramebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let fb = TileFramebuffer::new(width, height);
        let (notify_tx, notify_rx) = watch::channel(1);
        Self {
            inner: Arc::new(RwLock::new(fb)),
            notify_tx,
            notify_rx,
            frame_counter: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn notify_damage(&self) {
        let v = self.frame_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.notify_tx.send(v);
    }
}
