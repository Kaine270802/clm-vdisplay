use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Mouse button bit flags from RFB PointerEvent
pub const MOUSE_BTN_LEFT: u8 = 1 << 0;
pub const MOUSE_BTN_MIDDLE: u8 = 1 << 1;
pub const MOUSE_BTN_RIGHT: u8 = 1 << 2;
pub const MOUSE_WHEEL_UP: u8 = 1 << 3;
pub const MOUSE_WHEEL_DOWN: u8 = 1 << 4;
pub const MOUSE_WHEEL_LEFT: u8 = 1 << 5;
pub const MOUSE_WHEEL_RIGHT: u8 = 1 << 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Move {
        x: u16,
        y: u16,
    },
    ButtonDown {
        button: u8,
        x: u16,
        y: u16,
    },
    ButtonUp {
        button: u8,
        x: u16,
        y: u16,
    },
    Scroll {
        delta_x: i32,
        delta_y: i32,
        x: u16,
        y: u16,
    },
}

#[derive(Debug, Clone, Default)]
pub struct MouseState {
    pub x: u16,
    pub y: u16,
    pub button_mask: u8,
}

pub struct MouseRouter {
    state: Arc<RwLock<MouseState>>,
    event_tx: broadcast::Sender<MouseAction>,
}

impl Default for MouseRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseRouter {
    pub fn new() -> Self {
        // Trackpads (MacBook) emit PointerEvent bursts well above 256/s; a
        // small broadcast buffer Lagged the injector and froze remote hover.
        let (event_tx, _) = broadcast::channel(2048);
        Self {
            state: Arc::new(RwLock::new(MouseState::default())),
            event_tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MouseAction> {
        self.event_tx.subscribe()
    }

    pub fn get_state(&self) -> MouseState {
        self.state.read().clone()
    }

    pub fn get_position(&self) -> (u16, u16) {
        let state = self.state.read();
        (state.x, state.y)
    }

    pub fn get_button_mask(&self) -> u8 {
        self.state.read().button_mask
    }

    /// Process incoming RFB PointerEvent
    pub fn handle_pointer_event(&self, button_mask: u8, x: u16, y: u16) {
        let mut state = self.state.write();
        let prev_mask = state.button_mask;
        let prev_x = state.x;
        let prev_y = state.y;

        state.x = x;
        state.y = y;
        state.button_mask = button_mask & 0x07; // Keep standard buttons (1, 2, 4)

        // 1. Position move
        if x != prev_x || y != prev_y {
            let _ = self.event_tx.send(MouseAction::Move { x, y });
        }

        // 2. Standard button changes (Left, Middle, Right)
        for btn in [MOUSE_BTN_LEFT, MOUSE_BTN_MIDDLE, MOUSE_BTN_RIGHT] {
            let was_down = (prev_mask & btn) != 0;
            let is_down = (button_mask & btn) != 0;

            if !was_down && is_down {
                let _ = self
                    .event_tx
                    .send(MouseAction::ButtonDown { button: btn, x, y });
            } else if was_down && !is_down {
                let _ = self
                    .event_tx
                    .send(MouseAction::ButtonUp { button: btn, x, y });
            }
        }

        // 3. Scroll Wheel events (momentary flags)
        let mut delta_y = 0;
        let mut delta_x = 0;

        if (button_mask & MOUSE_WHEEL_UP) != 0 {
            delta_y += 1;
        }
        if (button_mask & MOUSE_WHEEL_DOWN) != 0 {
            delta_y -= 1;
        }
        if (button_mask & MOUSE_WHEEL_LEFT) != 0 {
            delta_x -= 1;
        }
        if (button_mask & MOUSE_WHEEL_RIGHT) != 0 {
            delta_x += 1;
        }

        if delta_x != 0 || delta_y != 0 {
            let _ = self.event_tx.send(MouseAction::Scroll {
                delta_x,
                delta_y,
                x,
                y,
            });
        }
    }
}
