//! Modern streaming backends (WebRTC ultra-low latency, CDP screencast direct pipe).

pub mod cdp_pipe;

#[cfg(feature = "webrtc")]
pub mod webrtc;

pub use cdp_pipe::CdpScreencastPipe;

#[cfg(feature = "webrtc")]
pub use webrtc::{WebRtcConfig, WebRtcStreamer};

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub enable_webrtc: bool,
    pub enable_cdp_screencast: bool,
}
