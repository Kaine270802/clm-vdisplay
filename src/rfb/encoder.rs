use crate::display::framebuffer::{PixelFormat, Rect, TileFramebuffer};
use crate::rfb::message::*;
use bytes::{BufMut, BytesMut};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;

/// Compact length encoding for Tight encoding (1, 2, or 3 bytes)
#[inline(always)]
pub fn write_compact_len(buf: &mut BytesMut, len: usize) {
    let mut val = len;
    let b0 = (val & 0x7F) as u8;
    val >>= 7;
    if val == 0 {
        buf.put_u8(b0);
        return;
    }
    buf.put_u8(b0 | 0x80);

    let b1 = (val & 0x7F) as u8;
    val >>= 7;
    if val == 0 {
        buf.put_u8(b1);
        return;
    }
    buf.put_u8(b1 | 0x80);
    buf.put_u8((val & 0xFF) as u8);
}

/// Encode a rectangle using RAW encoding directly into BytesMut
pub fn encode_raw_rect(
    fb: &TileFramebuffer,
    rect: &Rect,
    client_format: &PixelFormat,
    buf: &mut BytesMut,
) {
    let header = UpdateRectHeader::new(*rect, ENCODING_RAW);
    header.write_to(buf);

    let bpp = (client_format.bits_per_pixel / 8) as usize;
    let total_bytes = (rect.width as usize) * (rect.height as usize) * bpp;
    let start_pos = buf.len();
    buf.resize(start_pos + total_bytes, 0);

    fb.extract_rect_bytes_into(rect, client_format, &mut buf[start_pos..]);
}

/// Check if a pixel buffer is completely solid (all pixels identical) without allocating
#[inline]
fn get_solid_color_slice(data: &[u8], bpp: usize) -> Option<&[u8]> {
    if data.is_empty() || !data.len().is_multiple_of(bpp) {
        return None;
    }
    let first_pixel = &data[..bpp];
    for chunk in data.chunks_exact(bpp) {
        if chunk != first_pixel {
            return None;
        }
    }
    Some(first_pixel)
}

/// Encode a rectangle using Tight encoding (Solid Fill, Zlib Compression, or Raw Fallback)
pub fn encode_tight_rect(
    fb: &TileFramebuffer,
    rect: &Rect,
    client_format: &PixelFormat,
    buf: &mut BytesMut,
) {
    let pixel_data = fb.extract_rect_bytes(rect, client_format);
    let bpp = (client_format.bits_per_pixel / 8) as usize;

    let header = UpdateRectHeader::new(*rect, ENCODING_TIGHT);
    header.write_to(buf);

    // 1. Check for Solid Fill (1 pixel color)
    if let Some(solid_pixel) = get_solid_color_slice(&pixel_data, bpp) {
        buf.put_u8(0x80 | 0x08); // 0x80: Fill, stream 0
        buf.extend_from_slice(solid_pixel);
        return;
    }

    // 2. If small, use uncompressed stream
    if pixel_data.len() < 128 {
        buf.put_u8(0x00); // comp-ctl: stream 0, raw
        write_compact_len(buf, pixel_data.len());
        buf.extend_from_slice(&pixel_data);
        return;
    }

    // 3. Compress with Flate2 Zlib
    let mut encoder = ZlibEncoder::new(
        Vec::with_capacity(pixel_data.len() / 2),
        Compression::fast(),
    );
    if encoder.write_all(&pixel_data).is_ok() {
        if let Ok(compressed) = encoder.finish() {
            if compressed.len() < pixel_data.len() {
                buf.put_u8(0x00);
                write_compact_len(buf, compressed.len());
                buf.extend_from_slice(&compressed);
                return;
            }
        }
    }

    // Fallback uncompressed
    buf.put_u8(0x00);
    write_compact_len(buf, pixel_data.len());
    buf.extend_from_slice(&pixel_data);
}

/// Encode a rectangle using ZRLE (Zlib Run-Length Encoding)
pub fn encode_zrle_rect(
    fb: &TileFramebuffer,
    rect: &Rect,
    client_format: &PixelFormat,
    buf: &mut BytesMut,
) {
    let pixel_data = fb.extract_rect_bytes(rect, client_format);

    let mut encoder = ZlibEncoder::new(
        Vec::with_capacity(pixel_data.len() / 2),
        Compression::fast(),
    );
    let mut compressed_data = Vec::new();

    let mut tile_data = Vec::with_capacity(pixel_data.len() + 1);
    tile_data.push(0); // subencoding: raw
    tile_data.extend_from_slice(&pixel_data);

    if encoder.write_all(&tile_data).is_ok() {
        if let Ok(c) = encoder.finish() {
            compressed_data = c;
        }
    }

    if compressed_data.is_empty() {
        compressed_data = tile_data;
    }

    let header = UpdateRectHeader::new(*rect, ENCODING_ZRLE);
    header.write_to(buf);
    buf.put_u32(compressed_data.len() as u32);
    buf.extend_from_slice(&compressed_data);
}

/// Encode CopyRect rectangle
pub fn encode_copy_rect(rect: &Rect, src_x: u16, src_y: u16, buf: &mut BytesMut) {
    let header = UpdateRectHeader::new(*rect, ENCODING_COPY_RECT);
    header.write_to(buf);
    buf.put_u16(src_x);
    buf.put_u16(src_y);
}

/// Encode Pseudo-Encoding: DesktopSize
pub fn encode_pseudo_desktop_size(width: u16, height: u16, buf: &mut BytesMut) {
    let header =
        UpdateRectHeader::new(Rect::new(0, 0, width, height), PSEUDO_ENCODING_DESKTOP_SIZE);
    header.write_to(buf);
}

/// Encode Pseudo-Encoding: LastRect
pub fn encode_pseudo_last_rect(buf: &mut BytesMut) {
    let header = UpdateRectHeader::new(Rect::new(0, 0, 0, 0), PSEUDO_ENCODING_LAST_RECT);
    header.write_to(buf);
}

/// Encode Pseudo-Encoding: Cursor
pub fn encode_pseudo_cursor(
    hotspot_x: u16,
    hotspot_y: u16,
    width: u16,
    height: u16,
    rgba_pixels: &[u8],
    client_format: &PixelFormat,
    buf: &mut BytesMut,
) {
    let header = UpdateRectHeader::new(
        Rect::new(hotspot_x, hotspot_y, width, height),
        PSEUDO_ENCODING_CURSOR,
    );
    header.write_to(buf);

    let bpp = (client_format.bits_per_pixel / 8) as usize;
    let mut color_data = vec![0u8; (width as usize) * (height as usize) * bpp];
    let bitmask_bytes = (width as usize).div_ceil(8) * (height as usize);
    let mut bitmask = vec![0u8; bitmask_bytes];

    for y in 0..height as usize {
        for x in 0..width as usize {
            let idx = (y * width as usize + x) * 4;
            if idx + 3 < rgba_pixels.len() {
                let r = rgba_pixels[idx];
                let g = rgba_pixels[idx + 1];
                let b = rgba_pixels[idx + 2];
                let a = rgba_pixels[idx + 3];

                let out_idx = (y * width as usize + x) * bpp;
                if bpp == 4 {
                    color_data[out_idx] = r;
                    color_data[out_idx + 1] = g;
                    color_data[out_idx + 2] = b;
                    color_data[out_idx + 3] = a;
                } else if bpp == 3 {
                    color_data[out_idx] = r;
                    color_data[out_idx + 1] = g;
                    color_data[out_idx + 2] = b;
                }

                if a > 128 {
                    let mask_byte_idx = y * (width as usize).div_ceil(8) + (x / 8);
                    let bit_offset = 7 - (x % 8);
                    if mask_byte_idx < bitmask.len() {
                        bitmask[mask_byte_idx] |= 1 << bit_offset;
                    }
                }
            }
        }
    }

    buf.extend_from_slice(&color_data);
    buf.extend_from_slice(&bitmask);
}

/// Encode Pseudo-Encoding: DesktopName
pub fn encode_pseudo_desktop_name(name: &str, buf: &mut BytesMut) {
    let header = UpdateRectHeader::new(Rect::new(0, 0, 0, 0), PSEUDO_ENCODING_DESKTOP_NAME);
    header.write_to(buf);
    buf.put_u32(name.len() as u32);
    buf.extend_from_slice(name.as_bytes());
}
