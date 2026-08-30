use crate::display::framebuffer::SharedFramebuffer;
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};

/// Minimal Headless X11 Server fallback & Window Manager
pub struct HeadlessX11Server {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
    pub framebuffer: SharedFramebuffer,
    pub lock_file: PathBuf,
    pub socket_path: PathBuf,
}

impl HeadlessX11Server {
    pub fn new(display_num: u32, width: u32, height: u32, framebuffer: SharedFramebuffer) -> Self {
        let lock_file = PathBuf::from(format!("/tmp/.X{}-lock", display_num));
        let socket_path = PathBuf::from(format!("/tmp/.X11-unix/X{}", display_num));

        info!("Initializing Headless X11 Server on :{}", display_num);

        Self {
            display_num,
            width,
            height,
            framebuffer,
            lock_file,
            socket_path,
        }
    }

    /// Auto-maximize client window and update frame
    pub fn update_window_buffer(&self, x: u32, y: u32, w: u32, h: u32, data: &[u8], stride: usize) {
        {
            let mut fb = self.framebuffer.inner.write();
            fb.update_rect(x, y, w, h, data, stride);
        }
        self.framebuffer.notify_damage();
    }

    /// Clean up X11 lock files and socket on exit
    pub fn cleanup(&self) {
        if self.lock_file.exists() {
            if let Err(e) = fs::remove_file(&self.lock_file) {
                warn!("Failed to remove X11 lock file {:?}: {}", self.lock_file, e);
            }
        }
        if self.socket_path.exists() {
            if let Err(e) = fs::remove_file(&self.socket_path) {
                warn!(
                    "Failed to remove X11 socket file {:?}: {}",
                    self.socket_path, e
                );
            }
        }
    }
}

impl Drop for HeadlessX11Server {
    fn drop(&mut self) {
        self.cleanup();
    }
}
