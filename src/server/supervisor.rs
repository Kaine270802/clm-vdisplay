use crate::server::session::{DisplaySession, SessionConfig, SessionEvent};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub display_num: u32,
    pub width: u32,
    pub height: u32,
    pub rfb_port: u16,
    pub ws_port: Option<u16>,
    pub mode: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SupervisorCommand {
    Create {
        display_num: u32,
        width: u32,
        height: u32,
        rfb_port: Option<u16>,
        ws_port: Option<u16>,
        token: Option<String>,
        mode: Option<String>,
    },
    Stop {
        display_num: u32,
    },
    List,
    Get {
        display_num: u32,
    },
    InjectText {
        display_num: u32,
        text: String,
    },
    SetClipboard {
        display_num: u32,
        text: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SupervisorResponse {
    pub success: bool,
    pub message: Option<String>,
    pub displays: Option<Vec<DisplayInfo>>,
    pub display: Option<DisplayInfo>,
}

pub struct DisplaySupervisor {
    pub base_vnc_port: u16,
    pub control_socket_path: String,
    pub displays: Arc<RwLock<HashMap<u32, Arc<tokio::sync::Mutex<DisplaySession>>>>>,
    pub event_tx: mpsc::Sender<SessionEvent>,
    pub event_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<SessionEvent>>>,
    pub cancel_token: CancellationToken,
}

impl DisplaySupervisor {
    pub fn new(base_vnc_port: u16, control_socket_path: String) -> Self {
        let (event_tx, event_rx) = mpsc::channel(128);
        Self {
            base_vnc_port,
            control_socket_path,
            displays: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Allocate and launch a new display session using Scoped Lock Guards
    pub async fn create_display(
        &self,
        config: SessionConfig,
    ) -> anyhow::Result<Arc<tokio::sync::Mutex<DisplaySession>>> {
        let display_num = config.display_num;

        let mut session = DisplaySession::with_event_channel(config, Some(self.event_tx.clone()));
        session.start().await?;

        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
        {
            let mut guard = self.displays.write();
            guard.insert(display_num, session_arc.clone());
        } // guard dropped here immediately

        info!("Supervisor registered display :{}", display_num);
        Ok(session_arc)
    }

    /// Stop and remove a display session with Early Drop
    pub async fn stop_display(&self, display_num: u32) -> anyhow::Result<bool> {
        let session_arc = {
            let mut guard = self.displays.write();
            guard.remove(&display_num)
        }; // guard dropped here immediately

        if let Some(session_mutex) = session_arc {
            let mut session = session_mutex.lock().await;
            session.stop().await;
            info!("Supervisor stopped display :{}", display_num);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// List all active display sessions with Scoped Guard & Early Drop
    pub async fn list_displays(&self) -> Vec<DisplayInfo> {
        let sessions = {
            let guard = self.displays.read();
            guard.values().cloned().collect::<Vec<_>>()
        }; // guard dropped before iterating async locks

        let mut list = Vec::with_capacity(sessions.len());
        for s in sessions {
            let guard = s.lock().await;
            list.push(DisplayInfo {
                display_num: guard.config.display_num,
                width: guard.config.width,
                height: guard.config.height,
                rfb_port: guard.config.rfb_port,
                ws_port: guard.config.ws_port,
                mode: guard.config.mode.clone(),
            });
        }
        list
    }

    /// Run the Unix domain socket control server & background event reaper
    pub async fn run_control_server(&self) -> anyhow::Result<()> {
        let path = Path::new(&self.control_socket_path);
        if path.exists() {
            let _ = fs::remove_file(path);
        }

        let listener = UnixListener::bind(path)?;
        info!(
            "Supervisor IPC control server listening on unix:{}",
            self.control_socket_path
        );

        let event_rx = self.event_rx.clone();
        let displays_clone = self.displays.clone();
        let cancel_clone = self.cancel_token.child_token();

        // Background reaper task for unidirectional session termination events
        tokio::spawn(async move {
            let mut rx_guard = event_rx.lock().await;
            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    event = rx_guard.recv() => {
                        match event {
                            Some(SessionEvent::Terminated { display_num }) => {
                                let _removed = {
                                    let mut guard = displays_clone.write();
                                    guard.remove(&display_num)
                                };
                                info!("Session reaper cleaned up display :{}", display_num);
                            }
                            Some(SessionEvent::ClientDisconnected { session_id, display_num, peer_addr }) => {
                                info!("Session event: Client #{} disconnected from display :{} ({})", session_id, display_num, peer_addr);
                            }
                            Some(SessionEvent::ClientConnected { session_id, display_num, peer_addr }) => {
                                info!("Session event: Client #{} connected to display :{} ({})", session_id, display_num, peer_addr);
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    info!("Supervisor IPC server shutting down");
                    break;
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok((stream, _)) => {
                            let displays = self.displays.clone();
                            let base_port = self.base_vnc_port;
                            let event_tx = self.event_tx.clone();

                            tokio::spawn(async move {
                                if let Err(e) = handle_ipc_client(stream, displays, base_port, event_tx).await {
                                    warn!("IPC client error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            warn!("IPC accept error: {}", e);
                        }
                    }
                }
            }
        }

        if path.exists() {
            let _ = fs::remove_file(path);
        }

        Ok(())
    }
}

async fn handle_ipc_client(
    stream: UnixStream,
    displays: Arc<RwLock<HashMap<u32, Arc<tokio::sync::Mutex<DisplaySession>>>>>,
    base_vnc_port: u16,
    event_tx: mpsc::Sender<SessionEvent>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    while buf_reader.read_line(&mut line).await? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        let cmd: Result<SupervisorCommand, _> = serde_json::from_str(trimmed);
        let resp = match cmd {
            Ok(SupervisorCommand::Create {
                display_num,
                width,
                height,
                rfb_port,
                ws_port,
                token,
                mode,
            }) => {
                let actual_rfb = rfb_port.unwrap_or(base_vnc_port + (display_num % 100) as u16);
                let actual_mode = mode.unwrap_or_else(|| "hybrid".to_string());

                let config = SessionConfig {
                    display_num,
                    width,
                    height,
                    rfb_port: actual_rfb,
                    ws_port,
                    auth_token: token,
                    mode: actual_mode.clone(),
                };

                let mut session =
                    DisplaySession::with_event_channel(config, Some(event_tx.clone()));
                match session.start().await {
                    Ok(()) => {
                        let session_arc = Arc::new(tokio::sync::Mutex::new(session));
                        {
                            let mut guard = displays.write();
                            guard.insert(display_num, session_arc);
                        }
                        SupervisorResponse {
                            success: true,
                            message: Some(format!("Display :{} created", display_num)),
                            displays: None,
                            display: Some(DisplayInfo {
                                display_num,
                                width,
                                height,
                                rfb_port: actual_rfb,
                                ws_port,
                                mode: actual_mode,
                            }),
                        }
                    }
                    Err(e) => SupervisorResponse {
                        success: false,
                        message: Some(format!("Failed to start display: {}", e)),
                        displays: None,
                        display: None,
                    },
                }
            }
            Ok(SupervisorCommand::Stop { display_num }) => {
                let session_arc = {
                    let mut guard = displays.write();
                    guard.remove(&display_num)
                };
                if let Some(s) = session_arc {
                    let mut guard = s.lock().await;
                    guard.stop().await;
                    SupervisorResponse {
                        success: true,
                        message: Some(format!("Display :{} stopped", display_num)),
                        displays: None,
                        display: None,
                    }
                } else {
                    SupervisorResponse {
                        success: false,
                        message: Some(format!("Display :{} not found", display_num)),
                        displays: None,
                        display: None,
                    }
                }
            }
            Ok(SupervisorCommand::List) => {
                let sessions = {
                    let guard = displays.read();
                    guard.values().cloned().collect::<Vec<_>>()
                };
                let mut list = Vec::with_capacity(sessions.len());
                for s in sessions {
                    let guard = s.lock().await;
                    list.push(DisplayInfo {
                        display_num: guard.config.display_num,
                        width: guard.config.width,
                        height: guard.config.height,
                        rfb_port: guard.config.rfb_port,
                        ws_port: guard.config.ws_port,
                        mode: guard.config.mode.clone(),
                    });
                }
                SupervisorResponse {
                    success: true,
                    message: None,
                    displays: Some(list),
                    display: None,
                }
            }
            Ok(SupervisorCommand::Get { display_num }) => {
                let session_arc = {
                    let guard = displays.read();
                    guard.get(&display_num).cloned()
                };
                if let Some(s) = session_arc {
                    let guard = s.lock().await;
                    SupervisorResponse {
                        success: true,
                        message: None,
                        displays: None,
                        display: Some(DisplayInfo {
                            display_num: guard.config.display_num,
                            width: guard.config.width,
                            height: guard.config.height,
                            rfb_port: guard.config.rfb_port,
                            ws_port: guard.config.ws_port,
                            mode: guard.config.mode.clone(),
                        }),
                    }
                } else {
                    SupervisorResponse {
                        success: false,
                        message: Some("Display not found".to_string()),
                        displays: None,
                        display: None,
                    }
                }
            }
            Ok(SupervisorCommand::InjectText { display_num, text }) => {
                let session_arc = {
                    let guard = displays.read();
                    guard.get(&display_num).cloned()
                };
                if let Some(s) = session_arc {
                    let guard = s.lock().await;
                    guard.input_router.keyboard.inject_text(&text);
                    SupervisorResponse {
                        success: true,
                        message: Some("Text injected".to_string()),
                        displays: None,
                        display: None,
                    }
                } else {
                    SupervisorResponse {
                        success: false,
                        message: Some("Display not found".to_string()),
                        displays: None,
                        display: None,
                    }
                }
            }
            Ok(SupervisorCommand::SetClipboard { display_num, text }) => {
                let session_arc = {
                    let guard = displays.read();
                    guard.get(&display_num).cloned()
                };
                if let Some(s) = session_arc {
                    let guard = s.lock().await;
                    guard.input_router.clipboard.set_from_server(text);
                    SupervisorResponse {
                        success: true,
                        message: Some("Clipboard updated".to_string()),
                        displays: None,
                        display: None,
                    }
                } else {
                    SupervisorResponse {
                        success: false,
                        message: Some("Display not found".to_string()),
                        displays: None,
                        display: None,
                    }
                }
            }
            Err(e) => SupervisorResponse {
                success: false,
                message: Some(format!("Invalid command payload: {}", e)),
                displays: None,
                display: None,
            },
        };

        let mut resp_str = serde_json::to_string(&resp)?;
        resp_str.push('\n');
        writer.write_all(resp_str.as_bytes()).await?;
        writer.flush().await?;

        line.clear();
    }

    Ok(())
}
