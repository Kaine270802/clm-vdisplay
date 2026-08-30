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

        /// Automatically spawn and supervise X11 server (Xvfb) if display socket not found
        #[arg(long, default_value_t = false)]
        manage_x11: bool,

        /// Attach to existing X11 display without managing server lifecycle
        #[arg(long, default_value_t = false)]
        attach: bool,

        /// Custom path to Xvfb binary
        #[arg(long, default_value = "Xvfb")]
        xvfb_path: String,

        /// Custom extra arguments to pass to Xvfb (whitespace-separated)
        #[arg(long)]
        xvfb_args: Option<String>,

        /// Target capture frame rate (FPS)
        #[arg(long, default_value_t = 60)]
        fps: u32,
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
    pub manage_x11: bool,
    pub attach: bool,
    pub xvfb_path: String,
    pub xvfb_args: Option<Vec<String>>,
    pub fps: u32,
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
            manage_x11: true,
            attach: false,
            xvfb_path: "Xvfb".to_string(),
            xvfb_args: None,
            fps: 60,
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
        Self::from_start_args_full(
            display_str,
            resolution_str,
            rfb_port,
            ws_port,
            token,
            mode,
            metrics_port,
            false,
            false,
            "Xvfb".to_string(),
            None,
            60,
        )
    }

    pub fn from_start_args_full(
        display_str: &str,
        resolution_str: &str,
        rfb_port: u16,
        ws_port: Option<u16>,
        token: Option<String>,
        mode: &str,
        metrics_port: Option<u16>,
        manage_x11: bool,
        attach: bool,
        xvfb_path: String,
        xvfb_args: Option<String>,
        fps: u32,
    ) -> Self {
        let display_num = Self::parse_display_num(display_str);
        let (width, height, depth) = Self::parse_resolution(resolution_str);
        let enable_metrics = metrics_port.is_some();

        let parsed_args = xvfb_args.map(|s| {
            s.split_whitespace()
                .map(|item| item.to_string())
                .collect::<Vec<String>>()
        });

        let effective_manage_x11 = if attach {
            false
        } else if manage_x11 {
            true
        } else {
            true
        };

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
            manage_x11: effective_manage_x11,
            attach,
            xvfb_path,
            xvfb_args: parsed_args,
            fps: if fps == 0 { 60 } else { fps },
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
            .filter(|&w| w > 0)
            .unwrap_or(1920);
        let height = parts
            .get(1)
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&h| h > 0)
            .unwrap_or(1080);
        let depth = parts
            .get(2)
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|&d| d > 0)
            .unwrap_or(24);
        (width, height, depth)
    }
}
