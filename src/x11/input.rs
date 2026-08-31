use crate::input::{InputRouter, KeyAction, MouseAction};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

/// Injects mouse and keyboard events into X11 via XTest extension
pub struct X11InputInjector {
    conn: Arc<RustConnection>,
    root: u32,
    keysym_to_keycode: Arc<RwLock<HashMap<u32, u8>>>,
    prev_button_mask: Arc<RwLock<u8>>,
}

impl X11InputInjector {
    /// Initialize XTest connection on display :N and query keyboard mapping
    pub fn new(display_num: u32) -> anyhow::Result<Self> {
        let display_str = format!(":{}", display_num);
        let (conn, screen_num) = crate::x11::detector::X11Detector::connect_to_display(display_num)?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        // Verify XTest extension
        let xtest_ver = conn.xtest_get_version(2, 2)?.reply()?;
        info!(
            "XTest extension available on display {}: v{}.{}",
            display_str, xtest_ver.major_version, xtest_ver.minor_version
        );

        // Build KeySym -> KeyCode translation map
        let setup = conn.setup();
        let min_keycode = setup.min_keycode;
        let max_keycode = setup.max_keycode;
        let count = max_keycode.saturating_sub(min_keycode) + 1;

        let mapping_reply = conn.get_keyboard_mapping(min_keycode, count)?.reply()?;
        let keysyms_per_keycode = mapping_reply.keysyms_per_keycode as usize;

        let mut keysym_map = HashMap::new();
        for (i, chunk) in mapping_reply.keysyms.chunks(keysyms_per_keycode).enumerate() {
            let keycode = min_keycode.wrapping_add(i as u8);
            for &sym in chunk {
                if sym != 0 {
                    keysym_map.entry(sym).or_insert(keycode);
                }
            }
        }

        info!(
            "Loaded {} KeySym mappings from X11 display {}",
            keysym_map.len(),
            display_str
        );

        Ok(Self {
            conn: Arc::new(conn),
            root,
            keysym_to_keycode: Arc::new(RwLock::new(keysym_map)),
            prev_button_mask: Arc::new(RwLock::new(0)),
        })
    }

    /// Refresh keyboard mapping from X server (e.g. on layout change)
    pub fn refresh_keymap(&self) -> anyhow::Result<()> {
        let setup = self.conn.setup();
        let min_keycode = setup.min_keycode;
        let max_keycode = setup.max_keycode;
        let count = max_keycode.saturating_sub(min_keycode) + 1;

        let mapping_reply = self
            .conn
            .get_keyboard_mapping(min_keycode, count)?
            .reply()?;
        let keysyms_per_keycode = mapping_reply.keysyms_per_keycode as usize;

        let mut keysym_map = HashMap::new();
        for (i, chunk) in mapping_reply.keysyms.chunks(keysyms_per_keycode).enumerate() {
            let keycode = min_keycode.wrapping_add(i as u8);
            for &sym in chunk {
                if sym != 0 {
                    keysym_map.entry(sym).or_insert(keycode);
                }
            }
        }

        *self.keysym_to_keycode.write() = keysym_map;
        Ok(())
    }

    /// Send pointer motion and button state to X11 root window
    pub fn send_pointer_event(&self, x: u16, y: u16, button_mask: u8) -> anyhow::Result<()> {
        // 1. Send Motion Event
        self.conn.xtest_fake_input(
            xproto::MOTION_NOTIFY_EVENT,
            0,
            0,
            self.root,
            x as i16,
            y as i16,
            0,
        )?;

        // 2. Compute button diffs
        let mut prev = self.prev_button_mask.write();
        let prev_mask = *prev;
        *prev = button_mask;

        let buttons = [
            (1 << 0, 1u8), // Button 1: Left
            (1 << 1, 2u8), // Button 2: Middle
            (1 << 2, 3u8), // Button 3: Right
            (1 << 3, 4u8), // Button 4: Wheel Up
            (1 << 4, 5u8), // Button 5: Wheel Down
            (1 << 5, 6u8), // Button 6: Wheel Left
            (1 << 6, 7u8), // Button 7: Wheel Right
        ];

        for (flag, btn_id) in buttons {
            let was_down = (prev_mask & flag) != 0;
            let is_down = (button_mask & flag) != 0;

            if !was_down && is_down {
                self.conn.xtest_fake_input(
                    xproto::BUTTON_PRESS_EVENT,
                    btn_id,
                    0,
                    self.root,
                    x as i16,
                    y as i16,
                    0,
                )?;
            } else if was_down && !is_down {
                self.conn.xtest_fake_input(
                    xproto::BUTTON_RELEASE_EVENT,
                    btn_id,
                    0,
                    self.root,
                    x as i16,
                    y as i16,
                    0,
                )?;
            }
        }

        self.conn.flush()?;
        Ok(())
    }

    /// Send individual mouse button down/up event
    pub fn send_mouse_button(
        &self,
        button: u8,
        down: bool,
        x: u16,
        y: u16,
    ) -> anyhow::Result<()> {
        let event_type = if down {
            xproto::BUTTON_PRESS_EVENT
        } else {
            xproto::BUTTON_RELEASE_EVENT
        };
        self.conn.xtest_fake_input(
            event_type,
            button,
            0,
            self.root,
            x as i16,
            y as i16,
            0,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    /// Send mouse scroll step
    pub fn send_mouse_scroll(&self, delta_x: i32, delta_y: i32, x: u16, y: u16) -> anyhow::Result<()> {
        if delta_y > 0 {
            for _ in 0..delta_y {
                self.send_mouse_button(4, true, x, y)?;
                self.send_mouse_button(4, false, x, y)?;
            }
        } else if delta_y < 0 {
            for _ in 0..(-delta_y) {
                self.send_mouse_button(5, true, x, y)?;
                self.send_mouse_button(5, false, x, y)?;
            }
        }

        if delta_x > 0 {
            for _ in 0..delta_x {
                self.send_mouse_button(7, true, x, y)?;
                self.send_mouse_button(7, false, x, y)?;
            }
        } else if delta_x < 0 {
            for _ in 0..(-delta_x) {
                self.send_mouse_button(6, true, x, y)?;
                self.send_mouse_button(6, false, x, y)?;
            }
        }

        Ok(())
    }

    /// Send keyboard press / release event given an RFB / X11 KeySym
    pub fn send_key_event(&self, key_sym: u32, down: bool) -> anyhow::Result<()> {
        let keycode = {
            let map = self.keysym_to_keycode.read();
            map.get(&key_sym).copied()
        };

        if let Some(code) = keycode {
            let event_type = if down {
                xproto::KEY_PRESS_EVENT
            } else {
                xproto::KEY_RELEASE_EVENT
            };

            self.conn
                .xtest_fake_input(event_type, code, 0, self.root, 0, 0, 0)?;
            self.conn.flush()?;
        } else {
            warn!("Unmapped KeySym 0x{:04X}, cannot inject key event", key_sym);
        }

        Ok(())
    }

    /// Connect to InputRouter broadcast channels and forward client input directly into X11
    pub fn attach_input_router(
        self: Arc<Self>,
        input_router: &InputRouter,
        cancel_token: CancellationToken,
    ) {
        // Mouse routing task
        let injector_mouse = self.clone();
        let mut mouse_rx = input_router.mouse.subscribe();
        let cancel_mouse = cancel_token.child_token();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_mouse.cancelled() => break,
                    result = mouse_rx.recv() => {
                        let action = match result {
                            Ok(action) => action,
                            // Fast MacBook-trackpad motion can overrun the
                            // broadcast buffer; skip the burst and keep injecting.
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        };
                        match action {
                            MouseAction::Move { x, y } => {
                                let _ = injector_mouse.conn.xtest_fake_input(
                                    xproto::MOTION_NOTIFY_EVENT,
                                    0,
                                    0,
                                    injector_mouse.root,
                                    x as i16,
                                    y as i16,
                                    0,
                                );
                                let _ = injector_mouse.conn.flush();
                            }
                            MouseAction::ButtonDown { button, x, y } => {
                                let btn_id = match button {
                                    1 => 1,
                                    2 => 2,
                                    4 => 3,
                                    _ => 1,
                                };
                                let _ = injector_mouse.send_mouse_button(btn_id, true, x, y);
                            }
                            MouseAction::ButtonUp { button, x, y } => {
                                let btn_id = match button {
                                    1 => 1,
                                    2 => 2,
                                    4 => 3,
                                    _ => 1,
                                };
                                let _ = injector_mouse.send_mouse_button(btn_id, false, x, y);
                            }
                            MouseAction::Scroll { delta_x, delta_y, x, y } => {
                                let _ = injector_mouse.send_mouse_scroll(delta_x, delta_y, x, y);
                            }
                        }
                    }
                }
            }
        });

        // Keyboard routing task
        let injector_kb = self;
        let mut kb_rx = input_router.keyboard.subscribe();
        let cancel_kb = cancel_token;

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_kb.cancelled() => break,
                    result = kb_rx.recv() => {
                        match result {
                            Ok(KeyAction { down, key_sym, .. }) => {
                                let _ = injector_kb.send_key_event(key_sym, down);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }
}
