use crate::config::AppConfig;
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

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            display_num: 100,
            width: 1920,
            height: 1080,
            rfb_port: 5900,
            ws_port: Some(7861),
            auth_token: None,
            mode: "hybrid".to_string(),
        }
    }
}

impl From<&AppConfig> for SessionConfig {
    fn from(cfg: &AppConfig) -> Self {
        Self {
            display_num: cfg.display_num,
            width: cfg.width,
            height: cfg.height,
            rfb_port: cfg.rfb_port,
            ws_port: cfg.ws_port,
            auth_token: cfg.auth_token.clone(),
            mode: cfg.mode.clone(),
        }
    }
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
    pub manage_x11: bool,
    pub attach: bool,
    pub xvfb_path: String,
    pub xvfb_args: Option<Vec<String>>,
    pub fps: u32,
    tcp_handle: Option<JoinHandle<()>>,
    ws_handle: Option<JoinHandle<()>>,
}

impl DisplaySession {
    pub fn new(config: SessionConfig) -> Self {
        Self::with_event_channel(config, None)
    }

    pub fn from_app_config(cfg: &AppConfig) -> Self {
        let mut session = Self::new(SessionConfig::from(cfg));
        session.manage_x11 = cfg.manage_x11;
        session.attach = cfg.attach;
        session.xvfb_path = cfg.xvfb_path.clone();
        session.xvfb_args = cfg.xvfb_args.clone();
        session.fps = cfg.fps;
        session
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
            manage_x11: true,
            attach: false,
            xvfb_path: "Xvfb".to_string(),
            xvfb_args: None,
            fps: 60,
            tcp_handle: None,
            ws_handle: None,
        }
    }

    pub fn set_x11_options(
        &mut self,
        manage_x11: bool,
        attach: bool,
        xvfb_path: String,
        xvfb_args: Option<Vec<String>>,
        fps: u32,
    ) {
        self.manage_x11 = manage_x11;
        self.attach = attach;
        self.xvfb_path = xvfb_path;
        self.xvfb_args = xvfb_args;
        self.fps = fps;
    }

    /// Start the VNC and WebSocket servers for this display session
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let desktop_name = format!("CloakBrowser Display :{}", self.config.display_num);

        // 1. If X11 is enabled (x11 or hybrid mode), supervise/attach and start capture loop
        if let Some(ref x11) = self.display.x11 {
            x11.initialize(
                self.manage_x11,
                Some(self.xvfb_path.clone()),
                self.xvfb_args.clone(),
                self.fps,
                self.cancel_token.child_token(),
            )
            .await?;

            if let Some(injector) = x11.get_input_injector() {
                injector.attach_input_router(&self.input_router, self.cancel_token.child_token());
            }

            info!(
                "X11 socket ready: /tmp/.X11-unix/X{}",
                self.config.display_num
            );
        }

        // 2. Start TCP RFB Server
        let tcp_addr: SocketAddr = format!("0.0.0.0:{}", self.config.rfb_port).parse()?;
        let tcp_listener = tokio::net::TcpListener::bind(tcp_addr).await?;
        let tcp_server = TcpRfbServer::new(
            tcp_addr,
            desktop_name.clone(),
            self.config.auth_token.clone(),
            self.display.framebuffer.clone(),
            self.input_router.clone(),
            self.cancel_token.child_token(),
        );

        let tcp_task = tokio::spawn(async move {
            if let Err(e) = tcp_server.run_with_listener(tcp_listener).await {
                error!("TCP RFB Server exited with error: {}", e);
            }
        });
        self.tcp_handle = Some(tcp_task);

        // 3. Start WebSocket RFB Server (if configured)
        if let Some(ws_port) = self.config.ws_port {
            let ws_addr: SocketAddr = format!("0.0.0.0:{}", ws_port).parse()?;
            let ws_listener = tokio::net::TcpListener::bind(ws_addr).await?;
            let ws_server = WsRfbServer::new(
                ws_addr,
                desktop_name,
                self.config.auth_token.clone(),
                self.display.framebuffer.clone(),
                self.input_router.clone(),
                self.cancel_token.child_token(),
            );

            let ws_task = tokio::spawn(async move {
                if let Err(e) = ws_server.run_with_listener(ws_listener).await {
                    error!("WebSocket RFB Server exited with error: {}", e);
                }
            });
            self.ws_handle = Some(ws_task);
        }

        // Structured startup logging
        info!(
            "Display ready: :{} ({}x{}x24)",
            self.config.display_num, self.config.width, self.config.height
        );
        info!("VNC TCP listening on 0.0.0.0:{}", self.config.rfb_port);
        if let Some(ws_port) = self.config.ws_port {
            info!("WebSocket listening on 0.0.0.0:{}", ws_port);
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
