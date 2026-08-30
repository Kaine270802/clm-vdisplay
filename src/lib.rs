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

pub use config::AppConfig;
pub use display::VirtualDisplay;
pub use input::InputRouter;
pub use metrics::{MetricsRegistry, MetricsServer, GLOBAL_METRICS};
pub use rfb::{RfbProtocolEngine, RfbTransport, TcpRfbServer, WsRfbServer};
pub use server::{DisplayServer, DisplaySession, DisplaySupervisor, SessionConfig, SessionEvent};

/// Version of clm-vdisplay engine
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
