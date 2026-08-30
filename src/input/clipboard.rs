use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct ClipboardManager {
    text: Arc<RwLock<String>>,
    /// Channel for clipboard updates originated from clients (ClientCutText)
    client_tx: broadcast::Sender<String>,
    /// Channel for clipboard updates originated from server (ServerCutText)
    server_tx: broadcast::Sender<String>,
}

impl Default for ClipboardManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardManager {
    pub fn new() -> Self {
        let (client_tx, _) = broadcast::channel(64);
        let (server_tx, _) = broadcast::channel(64);
        Self {
            text: Arc::new(RwLock::new(String::new())),
            client_tx,
            server_tx,
        }
    }

    pub fn get_text(&self) -> String {
        self.text.read().clone()
    }

    /// Client sent new clipboard text via ClientCutText
    pub fn set_from_client(&self, new_text: impl Into<String>) {
        let val = new_text.into();
        {
            let mut text = self.text.write();
            *text = val.clone();
        }
        let _ = self.client_tx.send(val);
    }

    /// Server or local application updated clipboard, notify all VNC clients
    pub fn set_from_server(&self, new_text: impl Into<String>) {
        let val = new_text.into();
        {
            let mut text = self.text.write();
            *text = val.clone();
        }
        let _ = self.server_tx.send(val);
    }

    pub fn subscribe_server_updates(&self) -> broadcast::Receiver<String> {
        self.server_tx.subscribe()
    }

    pub fn subscribe_client_updates(&self) -> broadcast::Receiver<String> {
        self.client_tx.subscribe()
    }
}
