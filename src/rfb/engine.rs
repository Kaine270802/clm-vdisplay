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
        let client_ver_3_3 = ver_str.contains("003.003");
        let client_ver_3_7 = ver_str.contains("003.007");

        if client_ver_3_3 {
            // RFB 3.3: Server sends single u32 security type (None = 1)
            self.transport
                .send_bytes(&(SECURITY_TYPE_NONE as u32).to_be_bytes())
                .await?;
        } else {
            // RFB 3.7 / 3.8+: Server sends count (1) + list of security types ([SECURITY_TYPE_NONE])
            self.transport.send_bytes(&[1, SECURITY_TYPE_NONE]).await?;

            let mut sec_buf = [0u8; 1];
            self.transport
                .read_exact(&mut sec_buf, &mut staging)
                .await?;
            if sec_buf[0] != SECURITY_TYPE_NONE {
                if !client_ver_3_7 {
                    let _ = self
                        .transport
                        .send_bytes(&SECURITY_RESULT_FAILED.to_be_bytes())
                        .await;
                }
                return Err(anyhow::anyhow!("Security negotiation rejected"));
            }

            // In RFB 3.8, server sends SecurityResult OK (4 bytes 0)
            if !client_ver_3_7 {
                self.transport
                    .send_bytes(&SECURITY_RESULT_OK.to_be_bytes())
                    .await?;
            }
        }

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
        let mut supports_desktop_size = false;
        let mut _supports_desktop_name = false;
        let mut _supports_cursor = false;
        let mut continuous_updates = false;

        let mut client_width = width;
        let mut client_height = height;

        let mut send_buf = BytesMut::with_capacity(65536);
        let mut damage_rx = self.framebuffer.notify_rx.clone();
        let mut clipboard_rx = self.input_router.clipboard.subscribe_server_updates();

        let mut pending_update_rect: Option<Rect> = None;

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
                                supports_desktop_size = encs.contains(&PSEUDO_ENCODING_DESKTOP_SIZE);
                                _supports_desktop_name = encs.contains(&PSEUDO_ENCODING_DESKTOP_NAME);
                                _supports_cursor = encs.contains(&PSEUDO_ENCODING_CURSOR);
                                _client_encodings = encs;
                            }
                            ClientMessage::FramebufferUpdateRequest { incremental, rect } => {
                                if !incremental {
                                    send_framebuffer_update_stream(
                                        &mut self.transport,
                                        &mut send_buf,
                                        &self.framebuffer,
                                        &client_format,
                                        supports_tight,
                                        supports_zrle,
                                        supports_last_rect,
                                        supports_desktop_size,
                                        &mut client_width,
                                        &mut client_height,
                                        Some(rect),
                                        false,
                                        &self.metrics,
                                    ).await?;
                                    pending_update_rect = None;
                                } else {
                                    pending_update_rect = Some(rect);
                                    let sent = send_framebuffer_update_stream(
                                        &mut self.transport,
                                        &mut send_buf,
                                        &self.framebuffer,
                                        &client_format,
                                        supports_tight,
                                        supports_zrle,
                                        supports_last_rect,
                                        supports_desktop_size,
                                        &mut client_width,
                                        &mut client_height,
                                        Some(rect),
                                        true,
                                        &self.metrics,
                                    ).await?;
                                    if sent {
                                        pending_update_rect = None;
                                    }
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
                                } else {
                                    pending_update_rect = None;
                                    let end_msg = ServerMessage::end_of_continuous_updates();
                                    self.transport.send_bytes(&end_msg).await?;
                                }
                            }
                        }
                    }
                }

                // B. Framebuffer damage notification
                changed = damage_rx.changed() => {
                    if changed.is_ok() && (continuous_updates || pending_update_rect.is_some()) {
                        let target_rect = pending_update_rect;
                        let sent = send_framebuffer_update_stream(
                            &mut self.transport,
                            &mut send_buf,
                            &self.framebuffer,
                            &client_format,
                            supports_tight,
                            supports_zrle,
                            supports_last_rect,
                            supports_desktop_size,
                            &mut client_width,
                            &mut client_height,
                            target_rect,
                            true,
                            &self.metrics,
                        ).await?;
                        if sent && !continuous_updates {
                            pending_update_rect = None;
                        }
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
    _supports_tight: bool,
    _supports_zrle: bool,
    supports_last_rect: bool,
    supports_desktop_size: bool,
    client_width: &mut u16,
    client_height: &mut u16,
    target_rect: Option<Rect>,
    incremental: bool,
    metrics: &MetricsRegistry,
) -> anyhow::Result<bool> {
    send_buf.clear();
    let start_time = Instant::now();

    let (current_width, current_height, damaged_rects) = {
        let mut fb = framebuffer.inner.write();
        let cur_w = fb.width as u16;
        let cur_h = fb.height as u16;
        if (supports_desktop_size && (cur_w != *client_width || cur_h != *client_height))
            || !incremental
        {
            fb.mark_all_damaged();
        }
        (cur_w, cur_h, fb.detect_damage_tiles())
    };

    let size_changed =
        (current_width != *client_width || current_height != *client_height) && supports_desktop_size;

    let client_bounds = Rect::new(0, 0, *client_width, *client_height);
    let filtered_rects: Vec<Rect> = if let Some(req) = target_rect {
        damaged_rects
            .into_iter()
            .filter_map(|r| {
                r.intersection(&req).and_then(|ir| {
                    if !supports_desktop_size {
                        ir.intersection(&client_bounds)
                    } else {
                        Some(ir)
                    }
                })
            })
            .collect()
    } else if !supports_desktop_size {
        damaged_rects
            .into_iter()
            .filter_map(|r| r.intersection(&client_bounds))
            .collect()
    } else {
        damaged_rects
    };

    if filtered_rects.is_empty() && !size_changed {
        return Ok(false);
    }

    let rects_count = filtered_rects.len();

    {
        let fb_guard = framebuffer.inner.read();
        let num_rects = filtered_rects.len()
            + if size_changed { 1 } else { 0 }
            + if supports_last_rect { 1 } else { 0 };

        send_buf.extend_from_slice(&[SERVER_MSG_FRAMEBUFFER_UPDATE, 0]);
        send_buf.extend_from_slice(&(num_rects.min(65535) as u16).to_be_bytes());

        if size_changed {
            encode_pseudo_desktop_size(current_width, current_height, send_buf);
            *client_width = current_width;
            *client_height = current_height;
        }

        for rect in &filtered_rects {
            // Default to high-performance RAW stream with tile damage tracking:
            // delivers zero-copy byte alignment without zlib stream state desync.
            encode_raw_rect(&fb_guard, rect, client_format, send_buf);
        }

        if supports_last_rect {
            encode_pseudo_last_rect(send_buf);
        }
    }

    let bytes_len = send_buf.len();
    transport.send_bytes(send_buf).await?;

    let duration_us = start_time.elapsed().as_micros() as u64;
    metrics.record_frame_update(rects_count, bytes_len, duration_us);

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_send_framebuffer_update_stream_raw_and_desktop_size() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client_handle = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            socket
        });

        let mut client_socket = TcpStream::connect(addr).await.unwrap();
        let server_socket = client_handle.await.unwrap();

        let mut transport = RfbTransport::Tcp(server_socket);
        let mut send_buf = BytesMut::with_capacity(65536);
        let fb = SharedFramebuffer::new(128, 128);
        let format = PixelFormat::bgra32();
        let metrics = MetricsRegistry::new();

        let mut client_w = 128;
        let mut client_h = 128;

        // 1. Send full initial update (non-incremental) with LastRect
        let sent = send_framebuffer_update_stream(
            &mut transport,
            &mut send_buf,
            &fb,
            &format,
            false,
            false,
            true, // LastRect
            true, // DesktopSize
            &mut client_w,
            &mut client_h,
            None,
            false,
            &metrics,
        )
        .await
        .unwrap();

        assert!(sent);

        // Read and verify message from client_socket
        let mut recv_buf = vec![0u8; send_buf.len()];
        client_socket.read_exact(&mut recv_buf).await.unwrap();

        assert_eq!(recv_buf[0], SERVER_MSG_FRAMEBUFFER_UPDATE);
        assert_eq!(recv_buf[1], 0); // pad
        // 4 tiles (128x128 = 2x2 of 64x64) + 1 LastRect = 5 rects
        let num_rects = u16::from_be_bytes([recv_buf[2], recv_buf[3]]);
        assert_eq!(num_rects, 5);

        // 2. Incremental update with no changes -> returns false
        let sent_no_change = send_framebuffer_update_stream(
            &mut transport,
            &mut send_buf,
            &fb,
            &format,
            false,
            false,
            true,
            true,
            &mut client_w,
            &mut client_h,
            None,
            true,
            &metrics,
        )
        .await
        .unwrap();

        assert!(!sent_no_change);

        // 3. Resize framebuffer to 192x128 -> triggers DesktopSize pseudo-encoding
        {
            let mut fb_guard = fb.inner.write();
            fb_guard.resize(192, 128);
        }
        fb.notify_damage();

        let sent_resize = send_framebuffer_update_stream(
            &mut transport,
            &mut send_buf,
            &fb,
            &format,
            false,
            false,
            true,
            true, // supports DesktopSize
            &mut client_w,
            &mut client_h,
            None,
            true,
            &metrics,
        )
        .await
        .unwrap();

        assert!(sent_resize);
        assert_eq!(client_w, 192);
        assert_eq!(client_h, 128);

        let mut recv_buf2 = vec![0u8; send_buf.len()];
        client_socket.read_exact(&mut recv_buf2).await.unwrap();
        assert_eq!(recv_buf2[0], SERVER_MSG_FRAMEBUFFER_UPDATE);

        // First rect must be DesktopSize pseudo-encoding (12 bytes at offset 4)
        let header_ds = UpdateRectHeader::parse(&recv_buf2[4..16]).unwrap();
        assert_eq!(header_ds.rect, Rect::new(0, 0, 192, 128));
        assert_eq!(header_ds.encoding, PSEUDO_ENCODING_DESKTOP_SIZE);
    }

    #[tokio::test]
    async fn test_send_framebuffer_update_stream_bounds_clipping_without_desktop_size() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client_handle = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            socket
        });

        let mut client_socket = TcpStream::connect(addr).await.unwrap();
        let server_socket = client_handle.await.unwrap();

        let mut transport = RfbTransport::Tcp(server_socket);
        let mut send_buf = BytesMut::with_capacity(65536);
        let fb = SharedFramebuffer::new(64, 64);
        let format = PixelFormat::bgra32();
        let metrics = MetricsRegistry::new();

        let mut client_w = 64;
        let mut client_h = 64;

        // Framebuffer resized to 128x128 on server, but client DOES NOT support DesktopSize
        {
            let mut fb_guard = fb.inner.write();
            fb_guard.resize(128, 128);
        }
        fb.notify_damage();

        let sent = send_framebuffer_update_stream(
            &mut transport,
            &mut send_buf,
            &fb,
            &format,
            false,
            false,
            false, // no LastRect
            false, // no DesktopSize support!
            &mut client_w,
            &mut client_h,
            None,
            true,
            &metrics,
        )
        .await
        .unwrap();

        assert!(sent);
        // Client width and height must remain 64x64
        assert_eq!(client_w, 64);
        assert_eq!(client_h, 64);

        let mut recv_buf = vec![0u8; send_buf.len()];
        client_socket.read_exact(&mut recv_buf).await.unwrap();
        assert_eq!(recv_buf[0], SERVER_MSG_FRAMEBUFFER_UPDATE);

        // Only 1 tile inside (0, 0, 64, 64) is sent (not 4 tiles)
        let num_rects = u16::from_be_bytes([recv_buf[2], recv_buf[3]]);
        assert_eq!(num_rects, 1);

        let header = UpdateRectHeader::parse(&recv_buf[4..16]).unwrap();
        assert_eq!(header.rect, Rect::new(0, 0, 64, 64));
    }

    #[tokio::test]
    async fn test_rfb_handshake_versions_and_continuous_updates() {
        use crate::input::InputRouter;
        use tokio_util::sync::CancellationToken;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let cancel = CancellationToken::new();
        let cancel_child = cancel.child_token();

        let server_task = tokio::spawn(async move {
            let (socket, peer_addr) = listener.accept().await.unwrap();
            let engine = RfbProtocolEngine::new(
                1,
                peer_addr,
                RfbTransport::Tcp(socket),
                SharedFramebuffer::new(64, 64),
                InputRouter::new(),
                "TestDesktop".to_string(),
                None,
                cancel_child,
            );
            let _ = engine.run().await;
        });

        let mut client_socket = TcpStream::connect(addr).await.unwrap();

        // 1. Read Server Version (RFB 003.008\n)
        let mut ver_buf = [0u8; 12];
        client_socket.read_exact(&mut ver_buf).await.unwrap();
        assert_eq!(&ver_buf, RFB_VERSION_3_8);

        // 2. Client sends RFB 003.008\n
        client_socket.write_all(RFB_VERSION_3_8).await.unwrap();

        // 3. Server sends [1, SECURITY_TYPE_NONE]
        let mut sec_types = [0u8; 2];
        client_socket.read_exact(&mut sec_types).await.unwrap();
        assert_eq!(sec_types, [1, SECURITY_TYPE_NONE]);

        // 4. Client selects SECURITY_TYPE_NONE
        client_socket.write_all(&[SECURITY_TYPE_NONE]).await.unwrap();

        // 5. Server sends SecurityResult OK (4 bytes 0)
        let mut sec_res = [0u8; 4];
        client_socket.read_exact(&mut sec_res).await.unwrap();
        assert_eq!(sec_res, [0, 0, 0, 0]);

        // 6. Client sends ClientInit (shared = 1)
        client_socket.write_all(&[1]).await.unwrap();

        // 7. Server sends ServerInit (24 bytes + 11 name bytes)
        let mut init_hdr = [0u8; 24];
        client_socket.read_exact(&mut init_hdr).await.unwrap();
        let name_len = u32::from_be_bytes([init_hdr[20], init_hdr[21], init_hdr[22], init_hdr[23]]) as usize;
        let mut name_buf = vec![0u8; name_len];
        client_socket.read_exact(&mut name_buf).await.unwrap();
        assert_eq!(&name_buf, b"TestDesktop");

        // 8. Client sends EnableContinuousUpdates (enable = false)
        let mut disable_msg = vec![CLIENT_MSG_ENABLE_CONTINUOUS_UPDATES, 0];
        disable_msg.extend_from_slice(&0u16.to_be_bytes()); // x
        disable_msg.extend_from_slice(&0u16.to_be_bytes()); // y
        disable_msg.extend_from_slice(&64u16.to_be_bytes()); // w
        disable_msg.extend_from_slice(&64u16.to_be_bytes()); // h
        client_socket.write_all(&disable_msg).await.unwrap();

        // 9. Server must acknowledge with EndOfContinuousUpdates (message type 150)
        let mut end_msg = [0u8; 1];
        client_socket.read_exact(&mut end_msg).await.unwrap();
        assert_eq!(end_msg[0], SERVER_MSG_END_OF_CONTINUOUS_UPDATES);

        cancel.cancel();
        let _ = server_task.await;
    }
}
