use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;

/// Product cap for applied clipboard text (RFB parse may accept up to 10 MiB).
pub const CLIPBOARD_TEXT_CAP: usize = 256 * 1024;

/// Truncate to 256 KiB at a UTF-8 boundary. Never fails the RFB session.
pub fn cap_clipboard_text(text: impl Into<String>) -> String {
    let text = text.into();
    if text.len() <= CLIPBOARD_TEXT_CAP {
        return text;
    }
    warn!(
        "Clipboard text truncated from {} bytes to {} KiB product cap; RFB session kept",
        text.len(),
        CLIPBOARD_TEXT_CAP / 1024
    );
    let mut end = CLIPBOARD_TEXT_CAP;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

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
        let val = cap_clipboard_text(new_text);
        {
            let mut text = self.text.write();
            *text = val.clone();
        }
        let _ = self.client_tx.send(val);
    }

    /// Server or local application updated clipboard, notify all VNC clients
    pub fn set_from_server(&self, new_text: impl Into<String>) {
        let val = cap_clipboard_text(new_text);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_cap_truncates_over_256_kib() {
        let over = "a".repeat(CLIPBOARD_TEXT_CAP + 100);
        let capped = cap_clipboard_text(over);
        assert_eq!(capped.len(), CLIPBOARD_TEXT_CAP);
    }

    #[test]
    fn clipboard_cap_keeps_small_text() {
        let s = "hello\nclipboard".to_string();
        assert_eq!(cap_clipboard_text(s.clone()), s);
    }

    #[test]
    fn clipboard_manager_caps_client_and_server() {
        let mgr = ClipboardManager::new();
        mgr.set_from_client("x".repeat(CLIPBOARD_TEXT_CAP + 50));
        assert_eq!(mgr.get_text().len(), CLIPBOARD_TEXT_CAP);

        mgr.set_from_server("y".repeat(CLIPBOARD_TEXT_CAP + 1));
        assert_eq!(mgr.get_text().len(), CLIPBOARD_TEXT_CAP);
    }

    #[test]
    fn clipboard_cap_utf8_char_boundary() {
        // 'é' is 2 bytes; a cut in the middle of the last char must not panic.
        let mut s = "é".repeat(CLIPBOARD_TEXT_CAP / 2);
        s.push('é');
        assert!(s.len() > CLIPBOARD_TEXT_CAP);
        let capped = cap_clipboard_text(s);
        assert!(capped.len() <= CLIPBOARD_TEXT_CAP);
        assert!(capped.is_char_boundary(capped.len()));
    }
}
