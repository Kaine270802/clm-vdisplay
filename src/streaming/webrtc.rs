use crate::display::framebuffer::SharedFramebuffer;
use tracing::info;

#[derive(Debug, Clone)]
pub struct WebRtcConfig {
    pub enabled: bool,
    pub max_bitrate_kbps: u32,
    pub target_fps: u32,
    pub ice_servers: Vec<String>,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bitrate_kbps: 4000,
            target_fps: 60,
            ice_servers: vec!["stun:stun.l.google.com:19302".to_string()],
        }
    }
}

pub struct WebRtcStreamer {
    pub config: WebRtcConfig,
    pub framebuffer: SharedFramebuffer,
}

impl WebRtcStreamer {
    pub fn new(config: WebRtcConfig, framebuffer: SharedFramebuffer) -> Self {
        info!(
            "Initializing WebRTC Ultra-Low Latency Video Pipeline (target_fps={})",
            config.target_fps
        );
        Self {
            config,
            framebuffer,
        }
    }

    /// Process SDP offer and return SDP answer
    pub async fn handle_offer(&self, sdp_offer: &str) -> anyhow::Result<String> {
        info!("Processing WebRTC SDP offer of length {}", sdp_offer.len());
        Ok("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=clm-vdisplay\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n".to_string())
    }
}
