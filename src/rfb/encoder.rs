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
    let header = UpdateRectHeader::new(*rect, ENCODING_TIGHT);
    header.write_to(buf);

    let bpp = (client_format.bits_per_pixel / 8) as usize;
    // For TrueColour with depth 24 (both 24-bit and 32-bit bpp), Tight specification mandates 3 bytes per pixel (RGB24)
    let is_truecolour24 = client_format.depth == 24 && (bpp == 3 || bpp == 4);
    let pixel_data = if is_truecolour24 {
        fb.extract_rect_bytes(rect, &PixelFormat::rgb24())
    } else {
        fb.extract_rect_bytes(rect, client_format)
    };
    let tight_bpp = if is_truecolour24 { 3 } else { bpp.max(1) };

    // 1. Check for Solid Fill (1 pixel color)
    if let Some(solid_pixel) = get_solid_color_slice(&pixel_data, tight_bpp) {
        buf.put_u8(0x80); // 0x80: Fill compression type, stream 0
        buf.extend_from_slice(solid_pixel);
        return;
    }

    // 2. Small rectangle raw fallback (< 12 bytes per Tight spec)
    if pixel_data.len() < 12 {
        buf.put_u8(0x00); // comp-ctl: stream 0, raw (no reset, no filter)
        buf.extend_from_slice(&pixel_data);
        return;
    }

    // 3. Compress with Flate2 Zlib using stream 0 with reset bit (0x01) to avoid decompressor state desync
    let mut encoder = ZlibEncoder::new(
        Vec::with_capacity(pixel_data.len() / 2),
        Compression::fast(),
    );
    if encoder.write_all(&pixel_data).is_ok() {
        if let Ok(compressed) = encoder.finish() {
            buf.put_u8(0x01); // comp-ctl: stream 0, reset bit (0x01)
            write_compact_len(buf, compressed.len());
            buf.extend_from_slice(&compressed);
            return;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_compact_len_1_2_3_bytes() {
        let mut buf = BytesMut::new();

        // 1 byte (< 128)
        write_compact_len(&mut buf, 50);
        assert_eq!(&buf[..], &[50]);

        // 2 bytes (128..16383)
        buf.clear();
        write_compact_len(&mut buf, 300);
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0], (300 & 0x7F) as u8 | 0x80);
        assert_eq!(buf[1], (300 >> 7) as u8);

        // 3 bytes (>= 16384)
        buf.clear();
        write_compact_len(&mut buf, 20000);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], (20000 & 0x7F) as u8 | 0x80);
        assert_eq!(buf[1], ((20000 >> 7) & 0x7F) as u8 | 0x80);
        assert_eq!(buf[2], (20000 >> 14) as u8);
    }

    #[test]
    fn test_encode_raw_rect_byte_alignment() {
        let mut fb = TileFramebuffer::new(64, 64);
        let patch = vec![0x42; 64 * 64 * 4];
        fb.update_rect(0, 0, 64, 64, &patch, 64 * 4);

        let mut buf = BytesMut::new();
        let format = PixelFormat::bgra32();
        let rect = Rect::new(0, 0, 64, 64);

        encode_raw_rect(&fb, &rect, &format, &mut buf);

        // Header: 12 bytes + Data: 64 * 64 * 4 = 16384 bytes
        assert_eq!(buf.len(), 12 + 16384);

        let header = UpdateRectHeader::parse(&buf[..12]).expect("parsed header");
        assert_eq!(header.rect, rect);
        assert_eq!(header.encoding, ENCODING_RAW);
        assert_eq!(&buf[12..16], &[0x42, 0x42, 0x42, 0x42]);
    }

    #[test]
    fn test_encode_tight_rect_solid_fill() {
        let fb = TileFramebuffer::new(64, 64);
        let mut buf = BytesMut::new();
        let format = PixelFormat::bgra32();
        let rect = Rect::new(0, 0, 64, 64);

        encode_tight_rect(&fb, &rect, &format, &mut buf);

        // Header: 12 bytes
        // Solid fill flag: 1 byte (0x80)
        // Solid pixel (RGB24 for depth 24): 3 bytes (0, 0, 0)
        assert_eq!(buf.len(), 12 + 1 + 3);

        let header = UpdateRectHeader::parse(&buf[..12]).expect("parsed header");
        assert_eq!(header.encoding, ENCODING_TIGHT);
        assert_eq!(buf[12], 0x80); // Fill
        assert_eq!(&buf[13..16], &[0, 0, 0]);
    }

    #[test]
    fn test_pseudo_encodings_generation() {
        let mut buf = BytesMut::new();

        // DesktopSize
        encode_pseudo_desktop_size(1920, 1080, &mut buf);
        assert_eq!(buf.len(), 12);
        let h_ds = UpdateRectHeader::parse(&buf[..12]).unwrap();
        assert_eq!(h_ds.rect, Rect::new(0, 0, 1920, 1080));
        assert_eq!(h_ds.encoding, PSEUDO_ENCODING_DESKTOP_SIZE);

        // LastRect
        buf.clear();
        encode_pseudo_last_rect(&mut buf);
        assert_eq!(buf.len(), 12);
        let h_lr = UpdateRectHeader::parse(&buf[..12]).unwrap();
        assert_eq!(h_lr.rect, Rect::new(0, 0, 0, 0));
        assert_eq!(h_lr.encoding, PSEUDO_ENCODING_LAST_RECT);

        // DesktopName
        buf.clear();
        encode_pseudo_desktop_name("MyDesktop", &mut buf);
        assert_eq!(buf.len(), 12 + 4 + 9);
        let h_dn = UpdateRectHeader::parse(&buf[..12]).unwrap();
        assert_eq!(h_dn.encoding, PSEUDO_ENCODING_DESKTOP_NAME);
        assert_eq!(&buf[12..16], &9u32.to_be_bytes());
        assert_eq!(&buf[16..], b"MyDesktop");
    }

    #[test]
    fn test_encode_tight_rect_zlib_compression_and_decompression() {
        use flate2::read::ZlibDecoder;
        use std::io::Read;

        let mut fb = TileFramebuffer::new(64, 64);
        // Create non-solid gradient data
        let mut patch = vec![0u8; 64 * 64 * 4];
        for y in 0..64 {
            for x in 0..64 {
                let idx = (y * 64 + x) * 4;
                patch[idx] = (x * 4) as u8; // B
                patch[idx + 1] = (y * 4) as u8; // G
                patch[idx + 2] = ((x + y) * 2) as u8; // R
                patch[idx + 3] = 255; // A
            }
        }
        fb.update_rect(0, 0, 64, 64, &patch, 64 * 4);

        let mut buf = BytesMut::new();
        let format = PixelFormat::bgra32();
        let rect = Rect::new(0, 0, 64, 64);

        encode_tight_rect(&fb, &rect, &format, &mut buf);

        // Header: 12 bytes
        let header = UpdateRectHeader::parse(&buf[..12]).expect("parsed header");
        assert_eq!(header.rect, rect);
        assert_eq!(header.encoding, ENCODING_TIGHT);

        // comp_ctl: 0x01 (stream 0, reset flag)
        assert_eq!(buf[12], 0x01);

        // Read compact length
        let mut offset = 13;
        let b0 = buf[offset] as usize;
        offset += 1;
        let compressed_len = if b0 & 0x80 == 0 {
            b0
        } else {
            let b1 = buf[offset] as usize;
            offset += 1;
            if b1 & 0x80 == 0 {
                (b0 & 0x7F) | (b1 << 7)
            } else {
                let b2 = buf[offset] as usize;
                offset += 1;
                (b0 & 0x7F) | ((b1 & 0x7F) << 7) | (b2 << 14)
            }
        };

        let compressed_bytes = &buf[offset..offset + compressed_len];
        let mut decoder = ZlibDecoder::new(compressed_bytes);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).expect("decompression must succeed");

        // Depth 24 TrueColour -> 64 * 64 * 3 = 12288 bytes (RGB24)
        assert_eq!(decompressed.len(), 64 * 64 * 3);

        // Verify RGB24 pixel values
        let expected_rgb = fb.extract_rect_bytes(&rect, &PixelFormat::rgb24());
        assert_eq!(decompressed, expected_rgb);
    }

    #[test]
    fn test_encode_pseudo_cursor() {
        let mut buf = BytesMut::new();
        let rgba = vec![255u8, 0, 0, 255]; // 1 red pixel, opaque
        let format = PixelFormat::bgra32();
        encode_pseudo_cursor(0, 0, 1, 1, &rgba, &format, &mut buf);

        assert_eq!(buf.len(), 12 + 4 + 1); // 12 header + 4 color + 1 bitmask byte
        let header = UpdateRectHeader::parse(&buf[..12]).unwrap();
        assert_eq!(header.encoding, PSEUDO_ENCODING_CURSOR);
        assert_eq!(header.rect.width, 1);
        assert_eq!(header.rect.height, 1);
    }
}

