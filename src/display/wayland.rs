use crate::display::framebuffer::SharedFramebuffer;
use tracing::info;

/// Headless Wayland buffer manager & compositor integration
pub struct HeadlessWaylandCompositor {
    pub display_name: String,
    pub width: u32,
    pub height: u32,
    pub framebuffer: SharedFramebuffer,
}

impl HeadlessWaylandCompositor {
    pub fn new(
        display_name: String,
        width: u32,
        height: u32,
        framebuffer: SharedFramebuffer,
    ) -> Self {
        info!(
            "Initializing Headless Wayland Compositor on socket: {}",
            display_name
        );
        Self {
            display_name,
            width,
            height,
            framebuffer,
        }
    }

    /// Process incoming wl_shm buffer attachment directly into framebuffer (Zero-Copy)
    pub fn attach_shm_buffer(&self, x: u32, y: u32, w: u32, h: u32, data: &[u8], stride: usize) {
        {
            let mut fb = self.framebuffer.inner.write();
            fb.update_rect(x, y, w, h, data, stride);
        }
        self.framebuffer.notify_damage();
    }
}
