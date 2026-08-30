use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;

// Common X11 / RFB KeySyms
pub const KEYSYM_BACKSPACE: u32 = 0xFF08;
pub const KEYSYM_TAB: u32 = 0xFF09;
pub const KEYSYM_LINEFEED: u32 = 0xFF0A;
pub const KEYSYM_CLEAR: u32 = 0xFF0B;
pub const KEYSYM_RETURN: u32 = 0xFF0D;
pub const KEYSYM_PAUSE: u32 = 0xFF13;
pub const KEYSYM_SCROLL_LOCK: u32 = 0xFF14;
pub const KEYSYM_SYS_REQ: u32 = 0xFF15;
pub const KEYSYM_ESCAPE: u32 = 0xFF1B;
pub const KEYSYM_DELETE: u32 = 0xFFFF;

// Cursor Movement & Editing
pub const KEYSYM_HOME: u32 = 0xFF50;
pub const KEYSYM_LEFT: u32 = 0xFF51;
pub const KEYSYM_UP: u32 = 0xFF52;
pub const KEYSYM_RIGHT: u32 = 0xFF53;
pub const KEYSYM_DOWN: u32 = 0xFF54;
pub const KEYSYM_PAGE_UP: u32 = 0xFF55;
pub const KEYSYM_PAGE_DOWN: u32 = 0xFF56;
pub const KEYSYM_END: u32 = 0xFF57;
pub const KEYSYM_INSERT: u32 = 0xFF63;

// Function Keys
pub const KEYSYM_F1: u32 = 0xFFBE;
pub const KEYSYM_F2: u32 = 0xFFBF;
pub const KEYSYM_F3: u32 = 0xFFC0;
pub const KEYSYM_F4: u32 = 0xFFC1;
pub const KEYSYM_F5: u32 = 0xFFC2;
pub const KEYSYM_F6: u32 = 0xFFC3;
pub const KEYSYM_F7: u32 = 0xFFC4;
pub const KEYSYM_F8: u32 = 0xFFC5;
pub const KEYSYM_F9: u32 = 0xFFC6;
pub const KEYSYM_F10: u32 = 0xFFC7;
pub const KEYSYM_F11: u32 = 0xFFC8;
pub const KEYSYM_F12: u32 = 0xFFC9;

// Modifiers
pub const KEYSYM_SHIFT_L: u32 = 0xFFE1;
pub const KEYSYM_SHIFT_R: u32 = 0xFFE2;
pub const KEYSYM_CONTROL_L: u32 = 0xFFE3;
pub const KEYSYM_CONTROL_R: u32 = 0xFFE4;
pub const KEYSYM_CAPS_LOCK: u32 = 0xFFE5;
pub const KEYSYM_META_L: u32 = 0xFFE7;
pub const KEYSYM_META_R: u32 = 0xFFE8;
pub const KEYSYM_ALT_L: u32 = 0xFFE9;
pub const KEYSYM_ALT_R: u32 = 0xFFEA;
pub const KEYSYM_SUPER_L: u32 = 0xFFEB;
pub const KEYSYM_SUPER_R: u32 = 0xFFEC;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModifierState {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    pub caps_lock: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyAction {
    pub down: bool,
    pub key_sym: u32,
    pub modifiers: ModifierState,
    pub unicode_char: Option<char>,
}

pub struct KeyboardRouter {
    modifiers: Arc<RwLock<ModifierState>>,
    event_tx: broadcast::Sender<KeyAction>,
}

impl Default for KeyboardRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyboardRouter {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            modifiers: Arc::new(RwLock::new(ModifierState::default())),
            event_tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<KeyAction> {
        self.event_tx.subscribe()
    }

    pub fn get_modifiers(&self) -> ModifierState {
        *self.modifiers.read()
    }

    /// Resolve KeySym to Unicode character if applicable
    pub fn keysym_to_char(key_sym: u32) -> Option<char> {
        match key_sym {
            0x20..=0x7E => char::from_u32(key_sym),
            0x01000100..=0x0110FFFF => char::from_u32(key_sym - 0x01000000),
            KEYSYM_RETURN => Some('\n'),
            KEYSYM_TAB => Some('\t'),
            _ => None,
        }
    }

    /// Convert a Unicode char to corresponding X11/RFB KeySym
    pub fn char_to_keysym(ch: char) -> u32 {
        let code = ch as u32;
        if code <= 0x7E {
            code
        } else {
            0x01000000 | code
        }
    }

    /// Handle incoming RFB KeyEvent
    pub fn handle_key_event(&self, down: bool, key_sym: u32) {
        let mut mods = self.modifiers.write();

        match key_sym {
            KEYSYM_SHIFT_L | KEYSYM_SHIFT_R => mods.shift = down,
            KEYSYM_CONTROL_L | KEYSYM_CONTROL_R => mods.ctrl = down,
            KEYSYM_ALT_L | KEYSYM_ALT_R => mods.alt = down,
            KEYSYM_SUPER_L | KEYSYM_SUPER_R | KEYSYM_META_L | KEYSYM_META_R => mods.meta = down,
            KEYSYM_CAPS_LOCK if down => {
                mods.caps_lock = !mods.caps_lock;
            }
            _ => {}
        }

        let current_mods = *mods;
        let unicode_char = Self::keysym_to_char(key_sym);

        let action = KeyAction {
            down,
            key_sym,
            modifiers: current_mods,
            unicode_char,
        };

        let _ = self.event_tx.send(action);
    }

    /// Inject an entire text string directly by generating KeySym sequences
    pub fn inject_text(&self, text: &str) {
        for ch in text.chars() {
            let keysym = Self::char_to_keysym(ch);
            self.handle_key_event(true, keysym);
            self.handle_key_event(false, keysym);
        }
    }
}
