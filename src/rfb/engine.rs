use crate::display::framebuffer::{PixelFormat, Rect, SharedFramebuffer};
use crate::input::InputRouter;
use crate::metrics::{MetricsRegistry, GLOBAL_METRICS};
use crate::rfb::encoder::*;
use crate::rfb::message::*;
use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::WebSocketStream;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Unified transport abstraction over native TCP or WebSocket
pub enum RfbTransport {
    Tcp(TcpStream),
    WebSocket(Box<WebSocketStream<TcpStream>>),
}

impl RfbTransport {
    /// Send raw RFB bytes to client
    pub async fn send_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            RfbTransport::Tcp(ref mut socket) => {
                socket.write_all(data).await?;
                socket.flush().await?;
                Ok(())
            }
            RfbTransport::WebSocket(ref mut ws) => {
                ws.send(Message::Binary(data.to_vec()))
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::ConnectionAborted, e.to_string()))?;
                Ok(())
            }
        }
    }

    /// Read incoming bytes into buffer
    pub async fn recv_into(&mut self, read_buf: &mut BytesMut) -> io::Result<bool> {
        match self {
            RfbTransport::Tcp(ref mut socket) => {
                let mut chunk = [0u8; 4096];
                let n = socket.read(&mut chunk).await?;
                if n == 0 {
                    return Ok(false);
                }
                read_buf.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            RfbTransport::WebSocket(ref mut ws) => match ws.next().await {
                Some(Ok(msg)) => match msg {
                    Message::Binary(data) => {
                        read_buf.extend_from_slice(&data);
                        Ok(true)
                    }
                    Message::Ping(p) => {
                        let _ = ws.send(Message::Pong(p)).await;
                        Ok(true)
                    }
                    Message::Close(_) => Ok(false),
                    _ => Ok(true),
                },
                Some(Err(e)) => Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    e.to_string(),
                )),
                None => Ok(false),
            },
        }
    }

    /// Read exactly N bytes into buffer
    pub async fn read_exact(&mut self, buf: &mut [u8], staging: &mut BytesMut) -> io::Result<()> {
        while staging.len() < buf.len() {
            let ok = self.recv_into(staging).await?;
            if !ok {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Client disconnected during handshake",
                ));
            }
        }
        let chunk = staging.split_to(buf.len());
        buf.copy_from_slice(&chunk);
        Ok(())
    }
}

/// Unified RFB 3.8 Protocol Engine
pub struct RfbProtocolEngine {
    pub client_id: u64,
    pub peer_addr: SocketAddr,
    pub transport: RfbTransport,
    pub framebuffer: SharedFramebuffer,
    pub input_router: InputRouter,
    pub desktop_name: String,
    pub auth_token: Option<String>,
    pub cancel_token: CancellationToken,
    pub metrics: Arc<MetricsRegistry>,
}

impl RfbProtocolEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client_id: u64,
        peer_addr: SocketAddr,
        transport: RfbTransport,
        framebuffer: SharedFramebuffer,
        input_router: InputRouter,
        desktop_name: String,
        auth_token: Option<String>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            client_id,
            peer_addr,
            transport,
            framebuffer,
            input_router,
            desktop_name,
            auth_token,
            cancel_token,
            metrics: GLOBAL_METRICS.clone(),
        }
    }

    /// Run full RFB 3.8 handshake and client session loop
    pub async fn run(mut self) -> anyhow::Result<()> {
        self.metrics.inc_active_connections();
        let res = self.run_internal().await;
        self.metrics.dec_active_connections();
        res
    }

    async fn run_internal(&mut self) -> anyhow::Result<()> {
        let mut staging = BytesMut::with_capacity(4096);

        // 1. Protocol Version Handshake (RFB 003.008)
        self.transport.send_bytes(RFB_VERSION_3_8).await?;

        let mut ver_buf = [0u8; 12];
        self.transport
            .read_exact(&mut ver_buf, &mut staging)
            .await?;
        let ver_str = String::from_utf8_lossy(&ver_buf);
        info!(
            "Client #{} ({}) RFB version: {}",
            self.client_id,
            self.peer_addr,
            ver_str.trim()
        );

        // 2. Security Handshake
        self.transport.send_bytes(&[1, SECURITY_TYPE_NONE]).await?;

        let mut sec_buf = [0u8; 1];
        self.transport
            .read_exact(&mut sec_buf, &mut staging)
            .await?;
        if sec_buf[0] != SECURITY_TYPE_NONE {
            let _ = self
                .transport
                .send_bytes(&SECURITY_RESULT_FAILED.to_be_bytes())
                .await;
            return Err(anyhow::anyhow!("Security negotiation rejected"));
        }

        // Security Result OK
        self.transport
            .send_bytes(&SECURITY_RESULT_OK.to_be_bytes())
            .await?;

        // 3. ClientInit
        let mut shared_buf = [0u8; 1];
        self.transport
            .read_exact(&mut shared_buf, &mut staging)
            .await?;

        // 4. ServerInit
        let (width, height, default_format) = {
            let fb = self.framebuffer.inner.read();
            (fb.width as u16, fb.height as u16, fb.format)
        };

        let server_init_bytes =
            ServerMessage::server_init(width, height, &default_format, &self.desktop_name);
        self.transport.send_bytes(&server_init_bytes).await?;

        // 5. Client Event & Streaming Loop
        let mut client_format = default_format;
        let mut _client_encodings: Vec<i32> = Vec::new();
        let mut supports_tight = false;
        let mut supports_zrle = false;
        let mut supports_last_rect = false;
        let mut _supports_desktop_size = false;
        let mut _supports_cursor = false;
        let mut continuous_updates = false;

        let mut send_buf = BytesMut::with_capacity(65536);
        let mut damage_rx = self.framebuffer.notify_rx.clone();
        let mut clipboard_rx = self.input_router.clipboard.subscribe_server_updates();

        let mut pending_update_rect: Option<Rect> = None;
        let mut _pending_is_incremental = false;

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    break;
                }

                // A. Receive incoming client messages
                recv_res = self.transport.recv_into(&mut staging) => {
                    let ok = recv_res?;
                    if !ok {
                        break;
                    }

                    while let Some(msg) = ClientMessage::parse(&mut staging)? {
                        match msg {
                            ClientMessage::SetPixelFormat(fmt) => {
                                client_format = fmt;
                            }
                            ClientMessage::SetEncodings(encs) => {
                                supports_tight = encs.contains(&ENCODING_TIGHT);
                                supports_zrle = encs.contains(&ENCODING_ZRLE);
                                supports_last_rect = encs.contains(&PSEUDO_ENCODING_LAST_RECT);
                                _supports_desktop_size = encs.contains(&PSEUDO_ENCODING_DESKTOP_SIZE);
                                _supports_cursor = encs.contains(&PSEUDO_ENCODING_CURSOR);
                                _client_encodings = encs;
                            }
                            ClientMessage::FramebufferUpdateRequest { incremental, rect } => {
                                pending_update_rect = Some(rect);
                                _pending_is_incremental = incremental;

                                if !incremental {
                                    send_framebuffer_update_stream(
                                        &mut self.transport,
                                        &mut send_buf,
                                        &self.framebuffer,
                                        &client_format,
                                        supports_tight,
                                        supports_zrle,
                                        supports_last_rect,
                                        Some(rect),
                                        false,
                                        &self.metrics,
                                    ).await?;
                                    pending_update_rect = None;
                                }
                            }
                            ClientMessage::KeyEvent { down, key_sym } => {
                                self.input_router.keyboard.handle_key_event(down, key_sym);
                            }
                            ClientMessage::PointerEvent { button_mask, x, y } => {
                                self.input_router.mouse.handle_pointer_event(button_mask, x, y);
                            }
                            ClientMessage::ClientCutText(text) => {
                                self.input_router.clipboard.set_from_client(text);
                            }
                            ClientMessage::EnableContinuousUpdates { enable, rect } => {
                                continuous_updates = enable;
                                if enable {
                                    pending_update_rect = Some(rect);
                                    _pending_is_incremental = true;
                                }
                            }
                        }
                    }

                    if let Some(req_rect) = pending_update_rect {
                        send_framebuffer_update_stream(
                            &mut self.transport,
                            &mut send_buf,
                            &self.framebuffer,
                            &client_format,
                            supports_tight,
                            supports_zrle,
                            supports_last_rect,
                            Some(req_rect),
                            true,
                            &self.metrics,
                        ).await?;
                        pending_update_rect = None;
                    }
                }

                // B. Framebuffer damage notification
                changed = damage_rx.changed() => {
                    if changed.is_ok() && (continuous_updates || pending_update_rect.is_some()) {
                        let target_rect = pending_update_rect.take();
                        send_framebuffer_update_stream(
                            &mut self.transport,
                            &mut send_buf,
                            &self.framebuffer,
                            &client_format,
                            supports_tight,
                            supports_zrle,
                            supports_last_rect,
                            target_rect,
                            true,
                            &self.metrics,
                        ).await?;
                    }
                }

                // C. Clipboard synchronization from server
                clip_res = clipboard_rx.recv() => {
                    if let Ok(text) = clip_res {
                        let cut_bytes = ServerMessage::server_cut_text(&text);
                        self.transport.send_bytes(&cut_bytes).await?;
                    }
                }
            }
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_framebuffer_update_stream(
    transport: &mut RfbTransport,
    send_buf: &mut BytesMut,
    framebuffer: &SharedFramebuffer,
    client_format: &PixelFormat,
    supports_tight: bool,
    supports_zrle: bool,
    supports_last_rect: bool,
    target_rect: Option<Rect>,
    incremental: bool,
    metrics: &MetricsRegistry,
) -> anyhow::Result<()> {
    send_buf.clear();
    let start_time = Instant::now();

    let damaged_rects = {
        let mut fb = framebuffer.inner.write();
        if !incremental {
            fb.mark_all_damaged();
        }
        fb.detect_damage_tiles()
    };

    let filtered_rects: Vec<Rect> = if let Some(req) = target_rect {
        damaged_rects
            .into_iter()
            .filter(|r| r.intersects(&req))
            .collect()
    } else {
        damaged_rects
    };

    if filtered_rects.is_empty() {
        return Ok(());
    }

    let rects_count = filtered_rects.len();

    {
        let fb_guard = framebuffer.inner.read();
        let num_rects = filtered_rects.len() + if supports_last_rect { 1 } else { 0 };

        send_buf.extend_from_slice(&[SERVER_MSG_FRAMEBUFFER_UPDATE, 0]);
        send_buf.extend_from_slice(&(num_rects as u16).to_be_bytes());

        for rect in &filtered_rects {
            if supports_tight {
                encode_tight_rect(&fb_guard, rect, client_format, send_buf);
            } else if supports_zrle {
                encode_zrle_rect(&fb_guard, rect, client_format, send_buf);
            } else {
                encode_raw_rect(&fb_guard, rect, client_format, send_buf);
            }
        }

        if supports_last_rect {
            encode_pseudo_last_rect(send_buf);
        }
    }

    let bytes_len = send_buf.len();
    transport.send_bytes(send_buf).await?;

    let duration_us = start_time.elapsed().as_micros() as u64;
    metrics.record_frame_update(rects_count, bytes_len, duration_us);

    Ok(())
}
