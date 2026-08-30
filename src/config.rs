use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug, Clone, Serialize, Deserialize)]
#[command(
    name = "clm-vdisplay",
    author,
    version,
    about = "High-Performance Hybrid Virtual Display & VNC Streaming Engine in Rust",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug, Clone, Serialize, Deserialize)]
pub enum Commands {
    /// Start a standalone virtual display with VNC and WebSocket streaming
    Start {
        /// X11 display number string (e.g. :100)
        #[arg(short = 'd', long, default_value = ":100")]
        display: String,

        /// Resolution in WIDTHxHEIGHTxDEPTH format (e.g. 1920x1080x24)
        #[arg(short = 'r', long, default_value = "1920x1080x24")]
        resolution: String,

        /// Native TCP RFB (standard VNC) port to listen on
        #[arg(long, default_value_t = 5900)]
        rfb_port: u16,

        /// WebSocket RFB port for HTML5 noVNC client
        #[arg(long)]
        ws_port: Option<u16>,

        /// Authentication token for connections (mandatory for remote/WSS)
        #[arg(long)]
        token: Option<String>,

        /// Display mode: hybrid, wayland, or x11
        #[arg(short = 'm', long, default_value = "hybrid")]
        mode: String,

        /// Prometheus metrics & health check port
        #[arg(long)]
        metrics_port: Option<u16>,
    },

    /// Run the multi-display supervisor daemon
    Daemon {
        /// Base VNC port (5900)
        #[arg(long, default_value_t = 5900)]
        base_vnc_port: u16,

        /// IPC / Control socket path
        #[arg(long, default_value = "/tmp/clm-vdisplay.sock")]
        control_socket: String,

        /// Prometheus metrics & health check port
        #[arg(long)]
        metrics_port: Option<u16>,
    },
}

/// Unified Application Configuration loaded once at startup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub rfb_port: u16,
    pub ws_port: Option<u16>,
    pub auth_token: Option<String>,
    pub mode: String,
    pub base_vnc_port: u16,
    pub control_socket: String,
    pub metrics_port: Option<u16>,
    pub enable_metrics: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            display_num: 100,
            width: 1920,
            height: 1080,
            depth: 24,
            rfb_port: 5900,
            ws_port: Some(7861),
            auth_token: None,
            mode: "hybrid".to_string(),
            base_vnc_port: 5900,
            control_socket: "/tmp/clm-vdisplay.sock".to_string(),
            metrics_port: None,
            enable_metrics: false,
        }
    }
}

impl AppConfig {
    pub fn from_start_args(
        display_str: &str,
        resolution_str: &str,
        rfb_port: u16,
        ws_port: Option<u16>,
        token: Option<String>,
        mode: &str,
        metrics_port: Option<u16>,
    ) -> Self {
        let display_num = Self::parse_display_num(display_str);
        let (width, height, depth) = Self::parse_resolution(resolution_str);
        let enable_metrics = metrics_port.is_some();

        Self {
            display_num,
            width,
            height,
            depth,
            rfb_port,
            ws_port,
            auth_token: token,
            mode: mode.to_string(),
            base_vnc_port: 5900,
            control_socket: format!("/tmp/clm-vdisplay-{}.sock", display_num),
            metrics_port,
            enable_metrics,
        }
    }

    pub fn parse_display_num(disp: &str) -> u32 {
        let clean = disp.trim_start_matches(':');
        clean.parse::<u32>().unwrap_or(100)
    }

    pub fn parse_resolution(res: &str) -> (u32, u32, u8) {
        let parts: Vec<&str> = res.split('x').collect();
        let width = parts
            .first()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1920);
        let height = parts
            .get(1)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1080);
        let depth = parts
            .get(2)
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(24);
        (width, height, depth)
    }
}
