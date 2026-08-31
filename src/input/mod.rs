//! Unified input routing (mouse, keyboard, clipboard).

pub mod clipboard;
pub mod keyboard;
pub mod mouse;

pub use clipboard::{cap_clipboard_text, ClipboardManager, CLIPBOARD_TEXT_CAP};
pub use keyboard::{KeyAction, KeyboardRouter, ModifierState};
pub use mouse::{
    MouseAction, MouseRouter, MouseState, MOUSE_BTN_LEFT, MOUSE_BTN_MIDDLE, MOUSE_BTN_RIGHT,
    MOUSE_WHEEL_DOWN, MOUSE_WHEEL_LEFT, MOUSE_WHEEL_RIGHT, MOUSE_WHEEL_UP,
};

use std::sync::Arc;

#[derive(Clone)]
pub struct InputRouter {
    pub mouse: Arc<MouseRouter>,
    pub keyboard: Arc<KeyboardRouter>,
    pub clipboard: Arc<ClipboardManager>,
}

impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl InputRouter {
    pub fn new() -> Self {
        Self {
            mouse: Arc::new(MouseRouter::new()),
            keyboard: Arc::new(KeyboardRouter::new()),
            clipboard: Arc::new(ClipboardManager::new()),
        }
    }
}
