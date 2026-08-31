use crate::display::framebuffer::SharedFramebuffer;
use crate::input::InputRouter;
use crate::rfb::engine::{RfbProtocolEngine, RfbTransport};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

static WS_CLIENT_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct WsRfbServer {
    pub bind_addr: SocketAddr,
    pub desktop_name: String,
    pub auth_token: Option<String>,
    pub framebuffer: SharedFramebuffer,
    pub input_router: InputRouter,
    pub cancel_token: CancellationToken,
    pub capture_fps: Arc<AtomicU32>,
}

impl WsRfbServer {
    pub fn new(
        bind_addr: SocketAddr,
        desktop_name: String,
        auth_token: Option<String>,
        framebuffer: SharedFramebuffer,
        input_router: InputRouter,
        cancel_token: CancellationToken,
        capture_fps: Arc<AtomicU32>,
    ) -> Self {
        Self {
            bind_addr,
            desktop_name,
            auth_token,
            framebuffer,
            input_router,
            cancel_token,
            capture_fps,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.bind_addr).await?;
        self.run_with_listener(listener).await
    }

    pub async fn run_with_listener(&self, listener: TcpListener) -> anyhow::Result<()> {
        info!("WebSocket RFB (noVNC) listening on ws://{}", self.bind_addr);

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    info!("WebSocket RFB server shutting down on {}", self.bind_addr);
                    break;
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((stream, peer_addr)) => {
                            let client_id = WS_CLIENT_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
                            info!("Accepted WebSocket connection #{} from {}", client_id, peer_addr);

                            let fb = self.framebuffer.clone();
                            let input = self.input_router.clone();
                            let name = self.desktop_name.clone();
                            let token = self.auth_token.clone();
                            let cancel = self.cancel_token.child_token();
                            let capture_fps = self.capture_fps.clone();

                            tokio::spawn(async move {
                                if let Err(e) = handle_ws_accept(
                                    client_id,
                                    stream,
                                    peer_addr,
                                    fb,
                                    input,
                                    name,
                                    token,
                                    cancel,
                                    capture_fps,
                                )
                                .await
                                {
                                    info!("WebSocket client #{} disconnected: {}", client_id, e);
                                }
                            });
                        }
                        Err(e) => {
                            warn!("Error accepting WebSocket TCP connection: {}", e);
                            sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments, clippy::result_large_err)]
async fn handle_ws_accept(
    client_id: u64,
    stream: TcpStream,
    peer_addr: SocketAddr,
    framebuffer: SharedFramebuffer,
    input_router: InputRouter,
    desktop_name: String,
    auth_token: Option<String>,
    cancel_token: CancellationToken,
    capture_fps: Arc<AtomicU32>,
) -> anyhow::Result<()> {
    let token_clone = auth_token.clone();
    let callback = move |req: &Request,
                         mut resp: Response|
          -> Result<
        Response,
        tokio_tungstenite::tungstenite::http::Response<Option<String>>,
    > {
        if let Some(ref required_token) = token_clone {
            let uri = req.uri().to_string();
            let has_token = uri.contains(&format!("token={}", required_token))
                || req
                    .headers()
                    .get("Authorization")
                    .map(|h| h.to_str().unwrap_or(""))
                    .unwrap_or("")
                    .contains(required_token);
            if !has_token {
                let err_resp = tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED)
                    .body(Some("Unauthorized: Token Required".to_string()))
                    .unwrap();
                return Err(err_resp);
            }
        }

        if let Some(proto) = req.headers().get("Sec-WebSocket-Protocol") {
            if let Ok(proto_str) = proto.to_str() {
                if proto_str.contains("binary") {
                    resp.headers_mut().insert(
                        "Sec-WebSocket-Protocol",
                        tokio_tungstenite::tungstenite::http::HeaderValue::from_static("binary"),
                    );
                }
            }
        }
        Ok(resp)
    };

    let ws_stream = tokio_tungstenite::accept_hdr_async(stream, callback).await?;
    info!(
        "WebSocket handshake established for client #{} ({})",
        client_id, peer_addr
    );

    let engine = RfbProtocolEngine::new(
        client_id,
        peer_addr,
        RfbTransport::WebSocket(Box::new(ws_stream)),
        framebuffer,
        input_router,
        desktop_name,
        auth_token,
        cancel_token,
    )
    .with_capture_fps(capture_fps);

    engine.run().await
}
