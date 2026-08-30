use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Debug, Default)]
pub struct MetricsRegistry {
    pub active_connections: AtomicU64,
    pub total_connections: AtomicU64,
    pub total_frames_encoded: AtomicU64,
    pub total_damage_tiles: AtomicU64,
    pub total_bytes_sent: AtomicU64,
    pub last_encode_time_us: AtomicU64,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc_active_connections(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        self.total_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active_connections(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_frame_update(&self, tiles_count: usize, bytes_sent: usize, duration_us: u64) {
        self.total_frames_encoded.fetch_add(1, Ordering::Relaxed);
        self.total_damage_tiles
            .fetch_add(tiles_count as u64, Ordering::Relaxed);
        self.total_bytes_sent
            .fetch_add(bytes_sent as u64, Ordering::Relaxed);
        self.last_encode_time_us
            .store(duration_us, Ordering::Relaxed);
    }

    pub fn render_prometheus(&self) -> String {
        let active = self.active_connections.load(Ordering::Relaxed);
        let total_conn = self.total_connections.load(Ordering::Relaxed);
        let frames = self.total_frames_encoded.load(Ordering::Relaxed);
        let tiles = self.total_damage_tiles.load(Ordering::Relaxed);
        let bytes = self.total_bytes_sent.load(Ordering::Relaxed);
        let latency_us = self.last_encode_time_us.load(Ordering::Relaxed);

        format!(
            "# HELP clm_vdisplay_active_connections Number of active VNC/WS connections\n\
             # TYPE clm_vdisplay_active_connections gauge\n\
             clm_vdisplay_active_connections {}\n\n\
             # HELP clm_vdisplay_total_connections_count Total connections established\n\
             # TYPE clm_vdisplay_total_connections_count counter\n\
             clm_vdisplay_total_connections_count {}\n\n\
             # HELP clm_vdisplay_frames_encoded_total Total frames encoded\n\
             # TYPE clm_vdisplay_frames_encoded_total counter\n\
             clm_vdisplay_frames_encoded_total {}\n\n\
             # HELP clm_vdisplay_damage_tiles_total Total damage tiles tracked\n\
             # TYPE clm_vdisplay_damage_tiles_total counter\n\
             clm_vdisplay_damage_tiles_total {}\n\n\
             # HELP clm_vdisplay_bytes_sent_total Total bytes sent to clients\n\
             # TYPE clm_vdisplay_bytes_sent_total counter\n\
             clm_vdisplay_bytes_sent_total {}\n\n\
             # HELP clm_vdisplay_last_encode_duration_microseconds Last frame encoding latency\n\
             # TYPE clm_vdisplay_last_encode_duration_microseconds gauge\n\
             clm_vdisplay_last_encode_duration_microseconds {}\n",
            active, total_conn, frames, tiles, bytes, latency_us
        )
    }
}

pub static GLOBAL_METRICS: std::sync::LazyLock<Arc<MetricsRegistry>> =
    std::sync::LazyLock::new(|| Arc::new(MetricsRegistry::new()));

/// Lightweight HTTP health and metrics probe server
pub struct MetricsServer {
    pub port: u16,
    pub metrics: Arc<MetricsRegistry>,
    pub cancel_token: CancellationToken,
}

impl MetricsServer {
    pub fn new(port: u16, metrics: Arc<MetricsRegistry>, cancel_token: CancellationToken) -> Self {
        Self {
            port,
            metrics,
            cancel_token,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let addr: SocketAddr = format!("0.0.0.0:{}", self.port).parse()?;
        let listener = TcpListener::bind(addr).await?;
        info!("Metrics & Health probe server listening on http://{}", addr);

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    info!("Metrics server stopping on {}", addr);
                    break;
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((mut socket, _)) => {
                            let metrics = self.metrics.clone();
                            tokio::spawn(async move {
                                let mut buf = [0u8; 1024];
                                if let Ok(n) = socket.read(&mut buf).await {
                                    if n > 0 {
                                        let req_str = String::from_utf8_lossy(&buf[..n]);
                                        let path = req_str.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("/");

                                        let (status, content_type, body) = match path {
                                            "/health" | "/healthz" => (
                                                "200 OK",
                                                "application/json",
                                                format!("{{\"status\":\"ok\",\"version\":\"{}\"}}", env!("CARGO_PKG_VERSION")),
                                            ),
                                            "/metrics" => (
                                                "200 OK",
                                                "text/plain; version=0.0.4",
                                                metrics.render_prometheus(),
                                            ),
                                            _ => (
                                                "404 Not Found",
                                                "text/plain",
                                                "Not Found\n".to_string(),
                                            ),
                                        };

                                        let resp = format!(
                                            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                            status, content_type, body.len(), body
                                        );
                                        let _ = socket.write_all(resp.as_bytes()).await;
                                        let _ = socket.flush().await;
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            warn!("Metrics accept error: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
