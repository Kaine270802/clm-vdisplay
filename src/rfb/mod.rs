//! RFB 3.8 protocol implementation for native TCP VNC and WebSocket clients.

pub mod encoder;
pub mod engine;
pub mod message;
pub mod tcp_server;
pub mod ws_server;

pub use encoder::*;
pub use engine::{RfbProtocolEngine, RfbTransport};
pub use message::*;
pub use tcp_server::TcpRfbServer;
pub use ws_server::WsRfbServer;

#[derive(Debug, Clone)]
pub struct RfbServerConfig {
    pub rfb_port: u16,
    pub ws_port: Option<u16>,
    pub desktop_name: String,
    pub auth_token: Option<String>,
    pub width: u16,
    pub height: u16,
}
