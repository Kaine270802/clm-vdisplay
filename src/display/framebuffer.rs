use parking_lot::RwLock;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
            red_shift: 0,
            green_shift: 8,
            blue_shift: 16,
        }
    }

    /// Standard 24-bit BGR (3 bytes per pixel: B, G, R)
    pub fn bgr24() -> Self {
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
        self.x < other.x.saturating_add(other.width)
            && self.x.saturating_add(self.width) > other.x
            && self.y < other.y.saturating_add(other.height)
            && self.y.saturating_add(self.height) > other.y
    }

    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = (self.x.saturating_add(self.width)).min(other.x.saturating_add(other.width));
        let y2 = (self.y.saturating_add(self.height)).min(other.y.saturating_add(other.height));
        if x2 > x1 && y2 > y1 {
            Some(Rect::new(x1, y1, x2 - x1, y2 - y1))
        } else {
            None
        }
    }
}

/// Size of individual damage tracking tile (64x64)
pub const TILE_SIZE: usize = 64;

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
    pub buffer: Vec<u8>,
    /// Number of tiles in X and Y
    pub tiles_x: u32,
    pub tiles_y: u32,
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
        let tiles_x = (width as usize).div_ceil(TILE_SIZE) as u32;
        let tiles_y = (height as usize).div_ceil(TILE_SIZE) as u32;
        let total_tiles = (tiles_x * tiles_y) as usize;

        Self {
            width,
            height,
            stride,
            format: PixelFormat::bgra32(),
            buffer: vec![0u8; total_bytes],
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
        self.tiles_x = (width as usize).div_ceil(TILE_SIZE) as u32;
        self.tiles_y = (height as usize).div_ceil(TILE_SIZE) as u32;
        let total_tiles = (self.tiles_x * self.tiles_y) as usize;

        self.buffer.resize(total_bytes, 0);
        self.tile_hashes = vec![0u64; total_tiles];
        self.dirty_tiles = vec![true; total_tiles];
        self.frame_version = self.frame_version.wrapping_add(1);
    }

    /// Direct zero-copy slice of raw pixels
    pub fn raw_slice(&self) -> &[u8] {
        &self.buffer
    }

    /// Mark a specific tile as dirty
    pub fn mark_tile_dirty(&mut self, tx: u32, ty: u32) {
        if tx < self.tiles_x && ty < self.tiles_y {
            let idx = (ty * self.tiles_x + tx) as usize;
            if idx < self.dirty_tiles.len() {
                self.dirty_tiles[idx] = true;
                self.frame_version = self.frame_version.wrapping_add(1);
            }
        }
    }

    #[inline]
    fn mark_tiles_dirty_in_region(&mut self, x: u32, y: u32, clamp_w: usize, clamp_h: usize) {
        let tile_start_x = ((x as usize) / TILE_SIZE) as u32;
        let tile_end_x =
            (((x as usize + clamp_w - 1) / TILE_SIZE) as u32).min(self.tiles_x.saturating_sub(1));
        let tile_start_y = ((y as usize) / TILE_SIZE) as u32;
        let tile_end_y =
            (((y as usize + clamp_h - 1) / TILE_SIZE) as u32).min(self.tiles_y.saturating_sub(1));

        for ty in tile_start_y..=tile_end_y {
            for tx in tile_start_x..=tile_end_x {
                let idx = (ty * self.tiles_x + tx) as usize;
                if idx < self.dirty_tiles.len() {
                    self.dirty_tiles[idx] = true;
                }
            }
        }
        self.frame_version = self.frame_version.wrapping_add(1);
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

            if src_end <= src_data.len() && dst_end <= self.buffer.len() {
                self.buffer[dst_start..dst_end].copy_from_slice(&src_data[src_start..src_end]);
            }
        }

        self.mark_tiles_dirty_in_region(x, y, clamp_w, clamp_h);
    }

    /// Copy a damaged sub-rectangle from a full-frame buffer (e.g. MIT-SHM capture)
    pub fn update_rect_from_full_frame(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        full_frame: &[u8],
    ) {
        if x >= self.width || y >= self.height || w == 0 || h == 0 {
            return;
        }
        let clamp_w = (w).min(self.width - x) as usize;
        let clamp_h = (h).min(self.height - y) as usize;
        let bytes_per_pixel = 4;
        let row_bytes = clamp_w * bytes_per_pixel;

        for row in 0..clamp_h {
            let cur_y = (y as usize) + row;
            let offset = cur_y * self.stride + (x as usize) * bytes_per_pixel;
            let end = offset + row_bytes;

            if end <= full_frame.len() && end <= self.buffer.len() {
                self.buffer[offset..end].copy_from_slice(&full_frame[offset..end]);
            }
        }

        self.mark_tiles_dirty_in_region(x, y, clamp_w, clamp_h);
    }

    /// Mark all tiles as dirty (full screen refresh)
    pub fn mark_all_damaged(&mut self) {
        for dirty in self.dirty_tiles.iter_mut() {
            *dirty = true;
        }
        for hash in self.tile_hashes.iter_mut() {
            *hash = 0;
        }
        self.frame_version = self.frame_version.wrapping_add(1);
    }

    /// Check if any tiles are currently marked dirty
    pub fn has_dirty_tiles(&self) -> bool {
        self.dirty_tiles.iter().any(|&d| d)
    }

    /// Detect changed tiles using fast 64-bit differencing, update hashes, and return damaged rectangles
    pub fn detect_damage_tiles(&mut self) -> Vec<Rect> {
        let mut damaged_rects = Vec::new();
        let bytes_per_pixel = 4;

        for ty in 0..self.tiles_y {
            let tile_y = (ty as usize) * TILE_SIZE;
            let tile_h = TILE_SIZE.min(self.height as usize - tile_y);

            for tx in 0..self.tiles_x {
                let tile_idx = (ty * self.tiles_x + tx) as usize;
                if !self.dirty_tiles[tile_idx] {
                    continue;
                }

                let tile_x = (tx as usize) * TILE_SIZE;
                let tile_w = TILE_SIZE.min(self.width as usize - tile_x);

                // Compute tile hash by sampling rows
                let mut tile_hash: u64 = 0xcbf29ce484222325;
                for row in 0..tile_h {
                    let cur_y = tile_y + row;
                    let row_start = cur_y * self.stride + tile_x * bytes_per_pixel;
                    let row_end = row_start + tile_w * bytes_per_pixel;
                    if row_end <= self.buffer.len() {
                        let row_hash = hash_tile_data(&self.buffer[row_start..row_end]);
                        tile_hash ^= row_hash.wrapping_add((row as u64) << 32);
                        tile_hash = tile_hash.wrapping_mul(0x100000001b3);
                    }
                }

                self.dirty_tiles[tile_idx] = false;
                if tile_hash != self.tile_hashes[tile_idx] {
                    self.tile_hashes[tile_idx] = tile_hash;

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
        if rw == 0 || rh == 0 || bpp == 0 {
            return;
        }
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
                    if src_y < self.height as usize && rx < self.width as usize {
                        let copy_w = rw.min(self.width as usize - rx);
                        let src_row_start = src_y * self.stride + rx * 4;
                        let src_row_end = src_row_start + copy_w * 4;
                        if src_row_end <= self.buffer.len() {
                            let src_row = &self.buffer[src_row_start..src_row_end];
                            if is_bgra {
                                dst_row[..copy_w * 4].copy_from_slice(&src_row[..copy_w * 4]);
                            } else if is_rgba {
                                for col in 0..copy_w {
                                    let s = col * 4;
                                    let d = col * 4;
                                    dst_row[d] = src_row[s + 2]; // R
                                    dst_row[d + 1] = src_row[s + 1]; // G
                                    dst_row[d + 2] = src_row[s]; // B
                                    dst_row[d + 3] = src_row[s + 3]; // A
                                }
                            } else if is_rgb24 {
                                for col in 0..copy_w {
                                    let s = col * 4;
                                    let d = col * 3;
                                    dst_row[d] = src_row[s + 2]; // R
                                    dst_row[d + 1] = src_row[s + 1]; // G
                                    dst_row[d + 2] = src_row[s]; // B
                                }
                            } else if is_bgr24 {
                                for col in 0..copy_w {
                                    let s = col * 4;
                                    let d = col * 3;
                                    dst_row[d] = src_row[s]; // B
                                    dst_row[d + 1] = src_row[s + 1]; // G
                                    dst_row[d + 2] = src_row[s + 2]; // R
                                }
                            } else if bpp == 2 {
                                for col in 0..copy_w {
                                    let s = col * 4;
                                    let d = col * 2;
                                    let b = src_row[s] as u16;
                                    let g = src_row[s + 1] as u16;
                                    let r = src_row[s + 2] as u16;
                                    let r_val = ((r * target_format.red_max) / 255) << target_format.red_shift;
                                    let g_val = ((g * target_format.green_max) / 255) << target_format.green_shift;
                                    let b_val = ((b * target_format.blue_max) / 255) << target_format.blue_shift;
                                    let pixel16 = r_val | g_val | b_val;
                                    if target_format.big_endian_flag != 0 {
                                        dst_row[d..d + 2].copy_from_slice(&pixel16.to_be_bytes());
                                    } else {
                                        dst_row[d..d + 2].copy_from_slice(&pixel16.to_le_bytes());
                                    }
                                }
                            } else if bpp == 1 {
                                for col in 0..copy_w {
                                    let s = col * 4;
                                    let d = col;
                                    let b = src_row[s] as u16;
                                    let g = src_row[s + 1] as u16;
                                    let r = src_row[s + 2] as u16;
                                    let r_val = ((r * target_format.red_max) / 255) << target_format.red_shift;
                                    let g_val = ((g * target_format.green_max) / 255) << target_format.green_shift;
                                    let b_val = ((b * target_format.blue_max) / 255) << target_format.blue_shift;
                                    dst_row[d] = (r_val | g_val | b_val) as u8;
                                }
                            } else {
                                for col in 0..copy_w {
                                    let s = col * 4;
                                    let d = col * bpp;
                                    let b = src_row[s] as u32;
                                    let g = src_row[s + 1] as u32;
                                    let r = src_row[s + 2] as u32;
                                    let r_val = ((r * target_format.red_max as u32) / 255) << target_format.red_shift;
                                    let g_val = ((g * target_format.green_max as u32) / 255) << target_format.green_shift;
                                    let b_val = ((b * target_format.blue_max as u32) / 255) << target_format.blue_shift;
                                    let pixel32 = r_val | g_val | b_val;
                                    if bpp == 4 {
                                        if target_format.big_endian_flag != 0 {
                                            dst_row[d..d + 4].copy_from_slice(&pixel32.to_be_bytes());
                                        } else {
                                            dst_row[d..d + 4].copy_from_slice(&pixel32.to_le_bytes());
                                        }
                                    } else {
                                        for bi in 0..bpp.min(4) {
                                            dst_row[d + bi] = (pixel32 >> (bi * 8)) as u8;
                                        }
                                    }
                                }
                            }
                            if copy_w < rw {
                                dst_row[copy_w * bpp..dst_stride].fill(0);
                            }
                        }
                    }
                });
        } else {
            // Small tile sequential fast path
            for row in 0..rh {
                let src_y = ry + row;
                if src_y < self.height as usize && rx < self.width as usize {
                    let copy_w = rw.min(self.width as usize - rx);
                    let src_row_start = src_y * self.stride + rx * 4;
                    let src_row_end = src_row_start + copy_w * 4;
                    let dst_row_start = row * dst_stride;

                    if src_row_end <= self.buffer.len() && dst_row_start + dst_stride <= out.len() {
                        let src_row = &self.buffer[src_row_start..src_row_end];
                        let dst_row = &mut out[dst_row_start..dst_row_start + dst_stride];

                        if is_bgra {
                            dst_row[..copy_w * 4].copy_from_slice(&src_row[..copy_w * 4]);
                        } else if is_rgba {
                            for col in 0..copy_w {
                                let s = col * 4;
                                let d = col * 4;
                                dst_row[d] = src_row[s + 2]; // R
                                dst_row[d + 1] = src_row[s + 1]; // G
                                dst_row[d + 2] = src_row[s]; // B
                                dst_row[d + 3] = src_row[s + 3]; // A
                            }
                        } else if is_rgb24 {
                            for col in 0..copy_w {
                                let s = col * 4;
                                let d = col * 3;
                                dst_row[d] = src_row[s + 2]; // R
                                dst_row[d + 1] = src_row[s + 1]; // G
                                dst_row[d + 2] = src_row[s]; // B
                            }
                        } else if is_bgr24 {
                            for col in 0..copy_w {
                                let s = col * 4;
                                let d = col * 3;
                                dst_row[d] = src_row[s]; // B
                                dst_row[d + 1] = src_row[s + 1]; // G
                                dst_row[d + 2] = src_row[s + 2]; // R
                            }
                        } else if bpp == 2 {
                            for col in 0..copy_w {
                                let s = col * 4;
                                let d = col * 2;
                                let b = src_row[s] as u16;
                                let g = src_row[s + 1] as u16;
                                let r = src_row[s + 2] as u16;
                                let r_val = ((r * target_format.red_max) / 255) << target_format.red_shift;
                                let g_val = ((g * target_format.green_max) / 255) << target_format.green_shift;
                                let b_val = ((b * target_format.blue_max) / 255) << target_format.blue_shift;
                                let pixel16 = r_val | g_val | b_val;
                                if target_format.big_endian_flag != 0 {
                                    dst_row[d..d + 2].copy_from_slice(&pixel16.to_be_bytes());
                                } else {
                                    dst_row[d..d + 2].copy_from_slice(&pixel16.to_le_bytes());
                                }
                            }
                        } else if bpp == 1 {
                            for col in 0..copy_w {
                                let s = col * 4;
                                let d = col;
                                let b = src_row[s] as u16;
                                let g = src_row[s + 1] as u16;
                                let r = src_row[s + 2] as u16;
                                let r_val = ((r * target_format.red_max) / 255) << target_format.red_shift;
                                let g_val = ((g * target_format.green_max) / 255) << target_format.green_shift;
                                let b_val = ((b * target_format.blue_max) / 255) << target_format.blue_shift;
                                dst_row[d] = (r_val | g_val | b_val) as u8;
                            }
                        } else {
                            for col in 0..copy_w {
                                let s = col * 4;
                                let d = col * bpp;
                                let b = src_row[s] as u32;
                                let g = src_row[s + 1] as u32;
                                let r = src_row[s + 2] as u32;
                                let r_val = ((r * target_format.red_max as u32) / 255) << target_format.red_shift;
                                let g_val = ((g * target_format.green_max as u32) / 255) << target_format.green_shift;
                                let b_val = ((b * target_format.blue_max as u32) / 255) << target_format.blue_shift;
                                let pixel32 = r_val | g_val | b_val;
                                if bpp == 4 {
                                    if target_format.big_endian_flag != 0 {
                                        dst_row[d..d + 4].copy_from_slice(&pixel32.to_be_bytes());
                                    } else {
                                        dst_row[d..d + 4].copy_from_slice(&pixel32.to_le_bytes());
                                    }
                                } else {
                                    for bi in 0..bpp.min(4) {
                                        dst_row[d + bi] = (pixel32 >> (bi * 8)) as u8;
                                    }
                                }
                            }
                        }
                        if copy_w < rw {
                            dst_row[copy_w * bpp..dst_stride].fill(0);
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
    /// Set by RFB clients (and at construction) to force a full-screen grab
    /// before the capture loop waits on XDamage.
    full_capture_requested: Arc<AtomicBool>,
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
            full_capture_requested: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn notify_damage(&self) {
        let v = self.frame_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.notify_tx.send(v);
    }

    /// Request a full-screen capture on the next capture-loop tick.
    pub fn request_full_capture(&self) {
        self.full_capture_requested.store(true, Ordering::Release);
    }

    /// Consume a pending full-capture request. Returns true if a grab is due.
    pub fn take_full_capture_request(&self) -> bool {
        self.full_capture_requested.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_capture_request_flag() {
        let fb = SharedFramebuffer::new(64, 64);
        // Construction requests an initial full-screen grab.
        assert!(fb.take_full_capture_request());
        assert!(!fb.take_full_capture_request());
        fb.request_full_capture();
        assert!(fb.take_full_capture_request());
        assert!(!fb.take_full_capture_request());
    }

    #[test]
    fn test_rect_intersection() {
        let r1 = Rect::new(0, 0, 100, 100);
        let r2 = Rect::new(50, 50, 100, 100);
        assert!(r1.intersects(&r2));

        let inter = r1.intersection(&r2).expect("intersection expected");
        assert_eq!(inter, Rect::new(50, 50, 50, 50));

        let r3 = Rect::new(200, 200, 50, 50);
        assert!(!r1.intersects(&r3));
        assert_eq!(r1.intersection(&r3), None);
    }

    #[test]
    fn test_tile_framebuffer_64x64_damage_tracking() {
        let mut fb = TileFramebuffer::new(128, 128);
        assert_eq!(fb.tiles_x, 2);
        assert_eq!(fb.tiles_y, 2);

        // First detect: all tiles dirty
        let damaged = fb.detect_damage_tiles();
        assert_eq!(damaged.len(), 4);

        // Next detect without changes: 0 tiles dirty
        let damaged2 = fb.detect_damage_tiles();
        assert_eq!(damaged2.len(), 0);

        // Update single tile (top-left)
        let patch = vec![0xFF; 64 * 64 * 4];
        fb.update_rect(0, 0, 64, 64, &patch, 64 * 4);

        let damaged3 = fb.detect_damage_tiles();
        assert_eq!(damaged3.len(), 1);
        assert_eq!(damaged3[0], Rect::new(0, 0, 64, 64));
    }

    #[test]
    fn test_extract_rect_bytes_rgb24() {
        let mut fb = TileFramebuffer::new(64, 64);
        // BGRA format: B=10, G=20, R=30, A=255
        let pixel = [10u8, 20, 30, 255];
        let patch: Vec<u8> = pixel.iter().cycle().take(64 * 64 * 4).copied().collect();
        fb.update_rect(0, 0, 64, 64, &patch, 64 * 4);

        let rect = Rect::new(0, 0, 2, 2);
        let rgb_format = PixelFormat::rgb24();
        let extracted = fb.extract_rect_bytes(&rect, &rgb_format);

        assert_eq!(extracted.len(), 2 * 2 * 3); // 12 bytes
        // In RGB24: R=30, G=20, B=10
        assert_eq!(&extracted[0..3], &[30, 20, 10]);
        assert_eq!(&extracted[3..6], &[30, 20, 10]);
    }

    #[test]
    fn test_rect_intersection_overflow_boundary() {
        let r1 = Rect::new(0, 0, 100, 100);
        let r2 = Rect::new(u16::MAX - 50, u16::MAX - 50, 100, 100);
        // Ensure saturating arithmetic does not panic on integer overflow
        assert!(!r1.intersects(&r2));
        assert_eq!(r1.intersection(&r2), None);

        let r3 = Rect::new(u16::MAX - 20, u16::MAX - 20, 50, 50);
        let r4 = Rect::new(u16::MAX - 10, u16::MAX - 10, 50, 50);
        let inter = r3.intersection(&r4).expect("intersection expected");
        assert_eq!(inter.x, u16::MAX - 10);
        assert_eq!(inter.y, u16::MAX - 10);
        assert_eq!(inter.width, 10);
        assert_eq!(inter.height, 10);
    }

    #[test]
    fn test_extract_rect_zero_dimensions_no_panic() {
        let fb = TileFramebuffer::new(64, 64);
        let mut out = vec![0u8; 100];
        let rect = Rect::new(0, 0, 0, 0);
        let format = PixelFormat::bgra32();
        fb.extract_rect_bytes_into(&rect, &format, &mut out);

        let mut invalid_fmt = format;
        invalid_fmt.bits_per_pixel = 0;
        let rect2 = Rect::new(0, 0, 64, 64);
        fb.extract_rect_bytes_into(&rect2, &invalid_fmt, &mut out);
    }

    #[test]
    fn test_extract_rect_16bit_rgb565() {
        let mut fb = TileFramebuffer::new(64, 64);
        // BGRA format: B=0, G=255, R=0, A=255 -> Pure Green
        let pixel = [0u8, 255, 0, 255];
        let patch: Vec<u8> = pixel.iter().cycle().take(64 * 64 * 4).copied().collect();
        fb.update_rect(0, 0, 64, 64, &patch, 64 * 4);

        let mut rgb565 = PixelFormat {
            bits_per_pixel: 16,
            depth: 16,
            big_endian_flag: 0,
            true_colour_flag: 1,
            red_max: 31,
            green_max: 63,
            blue_max: 31,
            red_shift: 11,
            green_shift: 5,
            blue_shift: 0,
        };
        let rect = Rect::new(0, 0, 2, 2);
        let extracted = fb.extract_rect_bytes(&rect, &rgb565);
        assert_eq!(extracted.len(), 2 * 2 * 2); // 8 bytes
        let pixel_val = u16::from_le_bytes([extracted[0], extracted[1]]);
        // Green component: 63 << 5 = 0x07E0 = 2016
        assert_eq!(pixel_val, 0x07E0);

        // Test Big-Endian 16-bit
        rgb565.big_endian_flag = 1;
        let extracted_be = fb.extract_rect_bytes(&rect, &rgb565);
        let pixel_val_be = u16::from_be_bytes([extracted_be[0], extracted_be[1]]);
        assert_eq!(pixel_val_be, 0x07E0);
    }

    #[test]
    fn test_update_rect_from_full_frame_non_zero_coordinates() {
        let mut fb = TileFramebuffer::new(128, 128);
        // Create full-frame pattern where pixel (x, y) = (x as u8, y as u8, 0x55, 0xFF)
        let mut full_frame = vec![0u8; 128 * 128 * 4];
        for y in 0..128 {
            for x in 0..128 {
                let idx = (y * 128 + x) * 4;
                full_frame[idx] = x as u8; // B
                full_frame[idx + 1] = y as u8; // G
                full_frame[idx + 2] = 0x55; // R
                full_frame[idx + 3] = 0xFF; // A
            }
        }

        // Damage sub-region at x=64, y=64, w=32, h=32
        fb.update_rect_from_full_frame(64, 64, 32, 32, &full_frame);

        // Verify tile damage tracking marked tile (1, 1) dirty
        let damaged = fb.detect_damage_tiles();
        assert!(damaged.contains(&Rect::new(64, 64, 64, 64)));

        // Verify pixel data inside framebuffer at (64, 64) matches pattern
        let rect = Rect::new(64, 64, 2, 2);
        let extracted = fb.extract_rect_bytes(&rect, &PixelFormat::bgra32());
        assert_eq!(&extracted[0..4], &[64, 64, 0x55, 0xFF]);
        assert_eq!(&extracted[4..8], &[65, 64, 0x55, 0xFF]);
    }

    #[test]
    fn test_extract_rect_8bit_true_colour() {
        let mut fb = TileFramebuffer::new(64, 64);
        // BGRA format: B=0, G=0, R=255, A=255 -> Pure Red
        let pixel = [0u8, 0, 255, 255];
        let patch: Vec<u8> = pixel.iter().cycle().take(64 * 64 * 4).copied().collect();
        fb.update_rect(0, 0, 64, 64, &patch, 64 * 4);

        let rgb332 = PixelFormat {
            bits_per_pixel: 8,
            depth: 8,
            big_endian_flag: 0,
            true_colour_flag: 1,
            red_max: 7,
            green_max: 7,
            blue_max: 3,
            red_shift: 5,
            green_shift: 2,
            blue_shift: 0,
        };

        let rect = Rect::new(0, 0, 2, 2);
        let extracted = fb.extract_rect_bytes(&rect, &rgb332);
        assert_eq!(extracted.len(), 2 * 2); // 4 bytes
        // Pure Red: red_max (7) << 5 = 0b1110_0000 = 224
        assert_eq!(extracted[0], 224);
    }

    #[test]
    fn test_damage_tracking_static_frame_zero_bandwidth() {
        let mut fb = TileFramebuffer::new(128, 128);
        // Initial detection
        let initial_damaged = fb.detect_damage_tiles();
        assert_eq!(initial_damaged.len(), 4);

        // Subsequent detection on static framebuffer yields 0 damaged tiles
        let static_damaged = fb.detect_damage_tiles();
        assert_eq!(static_damaged.len(), 0);

        // Simulate 60 FPS capture loop writing identical browser frame
        let browser_frame = vec![128u8; 128 * 128 * 4];
        fb.update_rect_from_full_frame(0, 0, 128, 128, &browser_frame);
        let first_update = fb.detect_damage_tiles();
        assert_eq!(first_update.len(), 4);

        // Second update with IDENTICAL frame must emit 0 damage
        fb.update_rect_from_full_frame(0, 0, 128, 128, &browser_frame);
        let second_update = fb.detect_damage_tiles();
        assert_eq!(second_update.len(), 0);
    }

    #[test]
    fn test_child_window_render_produces_non_zero_pixels() {
        let mut fb = TileFramebuffer::new(256, 256);
        // Initially black background (0x00)
        let initial_pixels = fb.extract_rect_bytes(&Rect::new(0, 0, 256, 256), &PixelFormat::bgra32());
        assert!(initial_pixels.iter().all(|&b| b == 0));

        // Simulate Chrome child window rendering tab/address bar at (0, 0, 256, 60) with non-zero pixels
        let mut chrome_window = vec![0x33u8; 256 * 60 * 4];
        for i in 0..(256 * 60) {
            chrome_window[i * 4] = 0xF1;     // B
            chrome_window[i * 4 + 1] = 0xF2; // G
            chrome_window[i * 4 + 2] = 0xF3; // R
            chrome_window[i * 4 + 3] = 0xFF; // A
        }
        fb.update_rect(0, 0, 256, 60, &chrome_window, 256 * 4);

        let damaged = fb.detect_damage_tiles();
        assert!(!damaged.is_empty());

        let window_pixels = fb.extract_rect_bytes(&Rect::new(0, 0, 256, 60), &PixelFormat::bgra32());
        // Verify non-zero data
        assert!(window_pixels.iter().any(|&b| b != 0));
        assert_eq!(&window_pixels[0..4], &[0xF1, 0xF2, 0xF3, 0xFF]);
    }
}


