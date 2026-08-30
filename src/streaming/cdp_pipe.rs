use crate::display::framebuffer::{PixelFormat, SharedFramebuffer};
use image::{ImageBuffer, Rgb, Rgba};
use std::io::Cursor;

pub struct CdpScreencastPipe {
    pub browser_id: String,
    pub framebuffer: SharedFramebuffer,
    pub quality: u8,
}

impl CdpScreencastPipe {
    pub fn new(browser_id: String, framebuffer: SharedFramebuffer, quality: u8) -> Self {
        Self {
            browser_id,
            framebuffer,
            quality: quality.clamp(1, 100),
        }
    }

    /// Capture current screen as JPEG byte buffer for CDP screencast stream
    pub fn capture_jpeg(&self) -> anyhow::Result<Vec<u8>> {
        let fb = self.framebuffer.inner.read();
        let width = fb.width;
        let height = fb.height;
        let rgb_format = PixelFormat::rgb24();
        let full_rect = crate::display::framebuffer::Rect::new(0, 0, width as u16, height as u16);
        let rgb_bytes = fb.extract_rect_bytes(&full_rect, &rgb_format);

        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(width, height, rgb_bytes)
            .ok_or_else(|| anyhow::anyhow!("Failed to construct ImageBuffer"))?;

        let mut jpeg_bytes = Vec::new();
        let mut cursor = Cursor::new(&mut jpeg_bytes);
        img.write_to(&mut cursor, image::ImageFormat::Jpeg)?;

        Ok(jpeg_bytes)
    }

    /// Capture current screen as PNG byte buffer
    pub fn capture_png(&self) -> anyhow::Result<Vec<u8>> {
        let fb = self.framebuffer.inner.read();
        let width = fb.width;
        let height = fb.height;
        let rgba_format = PixelFormat::rgba32();
        let full_rect = crate::display::framebuffer::Rect::new(0, 0, width as u16, height as u16);
        let rgba_bytes = fb.extract_rect_bytes(&full_rect, &rgba_format);

        let img: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(width, height, rgba_bytes)
            .ok_or_else(|| anyhow::anyhow!("Failed to construct ImageBuffer"))?;

        let mut png_bytes = Vec::new();
        let mut cursor = Cursor::new(&mut png_bytes);
        img.write_to(&mut cursor, image::ImageFormat::Png)?;

        Ok(png_bytes)
    }
}
