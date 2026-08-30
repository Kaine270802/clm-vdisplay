//! Virtual display management, framebuffers, and compositor integration.

pub mod framebuffer;
pub mod wayland;
pub mod x11;

pub use framebuffer::{PixelFormat, Rect, SharedFramebuffer, TileFramebuffer, TILE_SIZE};
pub use wayland::HeadlessWaylandCompositor;
pub use x11::HeadlessX11Server;

use std::sync::Arc;

pub struct VirtualDisplay {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub mode: String,
    pub framebuffer: SharedFramebuffer,
    pub wayland: Option<Arc<HeadlessWaylandCompositor>>,
    pub x11: Option<Arc<HeadlessX11Server>>,
}

impl VirtualDisplay {
    pub fn new(display_num: u32, width: u32, height: u32, mode: &str) -> Self {
        let framebuffer = SharedFramebuffer::new(width, height);

        let wayland = if mode == "wayland" || mode == "hybrid" {
            Some(Arc::new(HeadlessWaylandCompositor::new(
                format!("wayland-{}", display_num),
                width,
                height,
                framebuffer.clone(),
            )))
        } else {
            None
        };

        let x11 = if mode == "x11" || mode == "hybrid" {
            Some(Arc::new(HeadlessX11Server::new(
                display_num,
                width,
                height,
                framebuffer.clone(),
            )))
        } else {
            None
        };

        Self {
            display_num,
            width,
            height,
            depth: 24,
            mode: mode.to_string(),
            framebuffer,
            wayland,
            x11,
        }
    }
}
