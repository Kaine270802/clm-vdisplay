//! # CLM-VDISPLAY
//! High-Performance Hybrid Virtual Display & VNC/WebRTC Streaming Engine in Rust.
//!
//! Replaces legacy `Xvfb` + `openbox` + `x11vnc` + `websockify` with a unified,
//! zero-copy, memory-safe single process daemon.

pub mod config;
pub mod display;
pub mod input;
pub mod metrics;
pub mod rfb;
pub mod server;
pub mod streaming;
pub mod x11;

pub use config::AppConfig;
pub use display::VirtualDisplay;
pub use input::InputRouter;
pub use metrics::{MetricsRegistry, MetricsServer, GLOBAL_METRICS};
pub use rfb::{RfbProtocolEngine, RfbTransport, TcpRfbServer, WsRfbServer};
pub use server::{DisplayServer, DisplaySession, DisplaySupervisor, SessionConfig, SessionEvent};
pub use x11::{
    DirtyBounds, DirtyTracker, ShmSegment, X11CaptureEngine, X11ClipboardBridge, X11Detector,
    X11DisplayState, X11InputInjector, X11ProcessGuard, X11Supervisor,
};

/// Version of clm-vdisplay engine
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
