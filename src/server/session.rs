use crate::display::VirtualDisplay;
use crate::input::InputRouter;
use crate::rfb::{TcpRfbServer, WsRfbServer};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
    pub rfb_port: u16,
    pub ws_port: Option<u16>,
    pub auth_token: Option<String>,
    pub mode: String,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    ClientConnected {
        session_id: u64,
        display_num: u32,
        peer_addr: SocketAddr,
    },
    ClientDisconnected {
        session_id: u64,
        display_num: u32,
        peer_addr: SocketAddr,
    },
    Terminated {
        display_num: u32,
    },
}

pub struct DisplaySession {
    pub config: SessionConfig,
    pub display: Arc<VirtualDisplay>,
    pub input_router: InputRouter,
    pub cancel_token: CancellationToken,
    pub event_tx: Option<mpsc::Sender<SessionEvent>>,
    tcp_handle: Option<JoinHandle<()>>,
    ws_handle: Option<JoinHandle<()>>,
}

impl DisplaySession {
    pub fn new(config: SessionConfig) -> Self {
        Self::with_event_channel(config, None)
    }

    pub fn with_event_channel(
        config: SessionConfig,
        event_tx: Option<mpsc::Sender<SessionEvent>>,
    ) -> Self {
        let display = Arc::new(VirtualDisplay::new(
            config.display_num,
            config.width,
            config.height,
            &config.mode,
        ));
        let input_router = InputRouter::new();
        let cancel_token = CancellationToken::new();

        Self {
            config,
            display,
            input_router,
            cancel_token,
            event_tx,
            tcp_handle: None,
            ws_handle: None,
        }
    }

    /// Start the VNC and WebSocket servers for this display session
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let desktop_name = format!("CloakBrowser Display :{}", self.config.display_num);

        // 1. Start TCP RFB Server
        let tcp_addr: SocketAddr = format!("0.0.0.0:{}", self.config.rfb_port).parse()?;
        let tcp_server = TcpRfbServer::new(
            tcp_addr,
            desktop_name.clone(),
            self.config.auth_token.clone(),
            self.display.framebuffer.clone(),
            self.input_router.clone(),
            self.cancel_token.child_token(),
        );

        let tcp_task = tokio::spawn(async move {
            if let Err(e) = tcp_server.run().await {
                error!("TCP RFB Server exited with error: {}", e);
            }
        });
        self.tcp_handle = Some(tcp_task);

        // 2. Start WebSocket RFB Server (if configured)
        if let Some(ws_port) = self.config.ws_port {
            let ws_addr: SocketAddr = format!("0.0.0.0:{}", ws_port).parse()?;
            let ws_server = WsRfbServer::new(
                ws_addr,
                desktop_name,
                self.config.auth_token.clone(),
                self.display.framebuffer.clone(),
                self.input_router.clone(),
                self.cancel_token.child_token(),
            );

            let ws_task = tokio::spawn(async move {
                if let Err(e) = ws_server.run().await {
                    error!("WebSocket RFB Server exited with error: {}", e);
                }
            });
            self.ws_handle = Some(ws_task);
        }

        info!(
            "DisplaySession :{} started successfully (RFB={}, WS={:?})",
            self.config.display_num, self.config.rfb_port, self.config.ws_port
        );

        Ok(())
    }

    /// Stop display session and cleanly abort background streaming servers
    pub async fn stop(&mut self) {
        info!("Stopping DisplaySession :{}", self.config.display_num);
        self.cancel_token.cancel();

        let tcp_handle = self.tcp_handle.take();
        let ws_handle = self.ws_handle.take();

        if let Some(handle) = tcp_handle {
            let _ = handle.await;
        }
        if let Some(handle) = ws_handle {
            let _ = handle.await;
        }

        if let Some(ref x11) = self.display.x11 {
            x11.cleanup();
        }

        if let Some(ref tx) = self.event_tx {
            let _ = tx
                .send(SessionEvent::Terminated {
                    display_num: self.config.display_num,
                })
                .await;
        }
    }
}
