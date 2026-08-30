use crate::display::framebuffer::SharedFramebuffer;
use crate::input::InputRouter;
use crate::rfb::engine::{RfbProtocolEngine, RfbTransport};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::net::TcpListener;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

static CLIENT_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct TcpRfbServer {
    pub bind_addr: SocketAddr,
    pub desktop_name: String,
    pub auth_token: Option<String>,
    pub framebuffer: SharedFramebuffer,
    pub input_router: InputRouter,
    pub cancel_token: CancellationToken,
}

impl TcpRfbServer {
    pub fn new(
        bind_addr: SocketAddr,
        desktop_name: String,
        auth_token: Option<String>,
        framebuffer: SharedFramebuffer,
        input_router: InputRouter,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            bind_addr,
            desktop_name,
            auth_token,
            framebuffer,
            input_router,
            cancel_token,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.bind_addr).await?;
        self.run_with_listener(listener).await
    }

    pub async fn run_with_listener(&self, listener: TcpListener) -> anyhow::Result<()> {
        info!("Native TCP RFB (vncviewer) listening on {}", self.bind_addr);

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    info!("TCP RFB server shutting down on {}", self.bind_addr);
                    break;
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((socket, peer_addr)) => {
                            let client_id = CLIENT_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
                            info!("Accepted TCP RFB connection #{} from {}", client_id, peer_addr);

                            let engine = RfbProtocolEngine::new(
                                client_id,
                                peer_addr,
                                RfbTransport::Tcp(socket),
                                self.framebuffer.clone(),
                                self.input_router.clone(),
                                self.desktop_name.clone(),
                                self.auth_token.clone(),
                                self.cancel_token.child_token(),
                            );

                            tokio::spawn(async move {
                                if let Err(e) = engine.run().await {
                                    info!("TCP RFB client #{} disconnected: {}", client_id, e);
                                }
                            });
                        }
                        Err(e) => {
                            warn!("Error accepting TCP connection: {}", e);
                            sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
