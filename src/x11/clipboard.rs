//! X11 CLIPBOARD (+ PRIMARY) bridge via XFixes / ICCCM.
//!
//! ClientCutText → become selection owner so Chrome Ctrl+V works.
//! XFixes SelectionNotify on CLIPBOARD → ConvertSelection → ServerCutText.

use crate::input::clipboard::{cap_clipboard_text, ClipboardManager};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{self, ConnectionExt as XFixesExt, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as XProtoExt, CreateWindowAux, EventMask, PropMode,
    SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

/// ICCCM / XFixes clipboard agent bound to one X display.
pub struct X11ClipboardBridge {
    conn: RustConnection,
    window: Window,
    atom_clipboard: Atom,
    atom_primary: Atom,
    atom_utf8: Atom,
    atom_targets: Atom,
    atom_text: Atom,
    atom_timestamp: Atom,
    atom_incr: Atom,
    atom_property: Atom,
    atom_string: Atom,
    held: String,
    pending_utf8: bool,
}

impl X11ClipboardBridge {
    /// Connect, create the agent window, intern atoms, subscribe XFixes.
    /// Blocks until the selection listener is ready (not fire-and-forget).
    pub fn new(display_num: u32) -> anyhow::Result<Self> {
        let display_str = format!(":{}", display_num);
        let (conn, screen_num) =
            crate::x11::detector::X11Detector::connect_to_display(display_num)?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;

        let xfixes_ver = conn.xfixes_query_version(5, 0)?.reply()?;
        info!(
            "XFixes clipboard bridge on display {}: v{}.{}",
            display_str, xfixes_ver.major_version, xfixes_ver.minor_version
        );

        let intern = |conn: &RustConnection, name: &[u8]| -> anyhow::Result<Atom> {
            Ok(conn.intern_atom(false, name)?.reply()?.atom)
        };

        let atom_clipboard = intern(&conn, b"CLIPBOARD")?;
        let atom_utf8 = intern(&conn, b"UTF8_STRING")?;
        let atom_targets = intern(&conn, b"TARGETS")?;
        let atom_text = intern(&conn, b"TEXT")?;
        let atom_timestamp = intern(&conn, b"TIMESTAMP")?;
        let atom_incr = intern(&conn, b"INCR")?;
        let atom_property = intern(&conn, b"CLM_CLIPBOARD")?;
        let atom_primary = Atom::from(u8::from(AtomEnum::PRIMARY));
        let atom_string = Atom::from(u8::from(AtomEnum::STRING));

        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;

        conn.xfixes_select_selection_input(
            window,
            atom_clipboard,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )?;
        conn.sync()?;

        info!(
            "X11 CLIPBOARD listener ready on display {} (window={:#x})",
            display_str, window
        );

        Ok(Self {
            conn,
            window,
            atom_clipboard,
            atom_primary,
            atom_utf8,
            atom_targets,
            atom_text,
            atom_timestamp,
            atom_incr,
            atom_property,
            atom_string,
            held: String::new(),
            pending_utf8: false,
        })
    }

    /// Event loop: ClientCutText → own CLIPBOARD+PRIMARY; XFixes → set_from_server.
    pub fn run(
        mut self,
        clipboard: Arc<ClipboardManager>,
        mut client_rx: broadcast::Receiver<String>,
        cancel: CancellationToken,
    ) {
        loop {
            if cancel.is_cancelled() {
                break;
            }

            loop {
                match client_rx.try_recv() {
                    Ok(text) => self.own_from_client(&text),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        self.own_from_client(&clipboard.get_text());
                    }
                    Err(broadcast::error::TryRecvError::Empty) => break,
                    Err(broadcast::error::TryRecvError::Closed) => return,
                }
            }

            let mut drained = 0usize;
            while drained < 64 {
                match self.conn.poll_for_event() {
                    Ok(Some(ev)) => {
                        drained += 1;
                        self.handle_event(ev, &clipboard);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!("X11 clipboard event poll failed: {}", e);
                        return;
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(8));
        }
    }

    fn own_from_client(&mut self, text: &str) {
        self.held = text.to_string();
        if let Err(e) =
            self.conn
                .set_selection_owner(self.window, self.atom_clipboard, CURRENT_TIME)
        {
            warn!("set_selection_owner CLIPBOARD failed: {}", e);
            return;
        }
        if let Err(e) = self
            .conn
            .set_selection_owner(self.window, self.atom_primary, CURRENT_TIME)
        {
            warn!("set_selection_owner PRIMARY failed: {}", e);
        }
        if let Err(e) = self.conn.flush() {
            warn!("clipboard flush after SetSelectionOwner failed: {}", e);
            return;
        }

        match self.conn.get_selection_owner(self.atom_clipboard) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => {
                    if reply.owner != self.window {
                        warn!(
                            "CLIPBOARD owner is {:#x}, expected our window {:#x}",
                            reply.owner, self.window
                        );
                    }
                }
                Err(e) => warn!("get_selection_owner CLIPBOARD reply failed: {}", e),
            },
            Err(e) => warn!("get_selection_owner CLIPBOARD request failed: {}", e),
        }
    }

    fn handle_event(&mut self, ev: Event, clipboard: &ClipboardManager) {
        match ev {
            Event::SelectionRequest(req) => {
                if let Err(e) = self.handle_selection_request(&req) {
                    warn!("SelectionRequest handler error: {}", e);
                }
            }
            Event::SelectionClear(_) => {}
            Event::SelectionNotify(sn) => {
                self.handle_convert_notify(&sn, clipboard);
            }
            Event::XfixesSelectionNotify(xn) => {
                self.handle_xfixes_notify(xn);
            }
            _ => {}
        }
    }

    fn handle_xfixes_notify(&mut self, xn: xfixes::SelectionNotifyEvent) {
        if xn.selection != self.atom_clipboard {
            return;
        }
        if xn.owner == self.window {
            return;
        }
        if xn.owner == NONE {
            return;
        }
        self.pending_utf8 = true;
        if let Err(e) = self.conn.convert_selection(
            self.window,
            self.atom_clipboard,
            self.atom_utf8,
            self.atom_property,
            CURRENT_TIME,
        ) {
            warn!("ConvertSelection UTF8_STRING failed: {}", e);
            self.pending_utf8 = false;
            return;
        }
        let _ = self.conn.flush();
    }

    fn handle_convert_notify(&mut self, sn: &SelectionNotifyEvent, clipboard: &ClipboardManager) {
        if sn.requestor != self.window || sn.selection != self.atom_clipboard {
            return;
        }
        if sn.property == NONE {
            if self.pending_utf8 {
                self.pending_utf8 = false;
                if let Err(e) = self.conn.convert_selection(
                    self.window,
                    self.atom_clipboard,
                    self.atom_string,
                    self.atom_property,
                    CURRENT_TIME,
                ) {
                    warn!("ConvertSelection STRING fallback failed: {}", e);
                } else {
                    let _ = self.conn.flush();
                }
            }
            return;
        }
        self.pending_utf8 = false;
        match self.read_property_text(sn.property) {
            Ok(Some(text)) => {
                clipboard.set_from_server(text);
            }
            Ok(None) => {}
            Err(e) => warn!("Failed to read converted CLIPBOARD: {}", e),
        }
        let _ = self.conn.delete_property(self.window, sn.property);
        let _ = self.conn.flush();
    }

    fn read_property_text(&self, property: Atom) -> anyhow::Result<Option<String>> {
        let reply = self
            .conn
            .get_property(
                false,
                self.window,
                property,
                AtomEnum::ANY,
                0,
                (crate::input::clipboard::CLIPBOARD_TEXT_CAP as u32 / 4) + 16,
            )?
            .reply()?;

        if reply.type_ == self.atom_incr {
            warn!(
                "CLIPBOARD INCR transfer ignored (product cap is {} KiB); RFB session kept",
                crate::input::clipboard::CLIPBOARD_TEXT_CAP / 1024
            );
            return Ok(None);
        }
        if reply.value.is_empty() {
            return Ok(Some(String::new()));
        }

        let raw = if reply.format == 8 {
            String::from_utf8_lossy(&reply.value).into_owned()
        } else {
            String::from_utf8_lossy(&reply.value).into_owned()
        };
        Ok(Some(cap_clipboard_text(raw)))
    }

    fn handle_selection_request(&self, req: &SelectionRequestEvent) -> anyhow::Result<()> {
        let property = if req.property == NONE {
            req.target
        } else {
            req.property
        };

        let success = if req.target == self.atom_targets {
            let targets = [
                self.atom_targets,
                self.atom_timestamp,
                self.atom_utf8,
                self.atom_string,
                self.atom_text,
            ];
            self.conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                property,
                AtomEnum::ATOM,
                &targets,
            )?;
            true
        } else if req.target == self.atom_timestamp {
            self.conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                property,
                AtomEnum::INTEGER,
                &[CURRENT_TIME],
            )?;
            true
        } else if req.target == self.atom_utf8
            || req.target == self.atom_string
            || req.target == self.atom_text
        {
            let ty = if req.target == self.atom_string {
                self.atom_string
            } else {
                self.atom_utf8
            };
            self.conn.change_property8(
                PropMode::REPLACE,
                req.requestor,
                property,
                ty,
                self.held.as_bytes(),
            )?;
            true
        } else {
            false
        };

        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property: if success { property } else { NONE },
        };
        self.conn
            .send_event(false, req.requestor, EventMask::NO_EVENT, notify)?;
        self.conn.flush()?;
        Ok(())
    }
}

/// Spawn the clipboard bridge on a blocking thread. Waits until XFixes is subscribed
/// (same race as the old fire-and-forget XTest injector).
pub async fn start_clipboard_bridge(
    display_num: u32,
    clipboard: Arc<ClipboardManager>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        match X11ClipboardBridge::new(display_num) {
            Ok(bridge) => {
                // Subscribe before signalling ready so ClientCutText cannot be dropped.
                let client_rx = clipboard.subscribe_client_updates();
                let _ = ready_tx.send(Ok(()));
                bridge.run(clipboard, client_rx, cancel);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    });

    match ready_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow::anyhow!(
            "X11 clipboard bridge task dropped before becoming ready"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_bridge_skips_without_display() {
        if std::env::var_os("DISPLAY").is_none() {
            eprintln!("skip: DISPLAY unset");
            return;
        }
        // Best-effort: a live X server is not required for unit CI.
        match X11ClipboardBridge::new(99_001) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("skip: no usable X11 display ({e})");
            }
        }
    }
}
