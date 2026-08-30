use crate::display::framebuffer::{PixelFormat, Rect};
use bytes::{Buf, BufMut, BytesMut};
use std::io;

pub const RFB_VERSION_3_8: &[u8; 12] = b"RFB 003.008\n";
pub const RFB_VERSION_3_7: &[u8; 12] = b"RFB 003.007\n";
pub const RFB_VERSION_3_3: &[u8; 12] = b"RFB 003.003\n";

// Security Types
pub const SECURITY_TYPE_INVALID: u8 = 0;
pub const SECURITY_TYPE_NONE: u8 = 1;
pub const SECURITY_TYPE_VNC_AUTH: u8 = 2;

// Security Results
pub const SECURITY_RESULT_OK: u32 = 0;
pub const SECURITY_RESULT_FAILED: u32 = 1;

// Encoding types
pub const ENCODING_RAW: i32 = 0;
pub const ENCODING_COPY_RECT: i32 = 1;
pub const ENCODING_RRE: i32 = 2;
pub const ENCODING_HEXTILE: i32 = 5;
pub const ENCODING_TIGHT: i32 = 7;
pub const ENCODING_ZRLE: i32 = 16;

// Pseudo-encodings
pub const PSEUDO_ENCODING_CURSOR: i32 = -239;
pub const PSEUDO_ENCODING_DESKTOP_SIZE: i32 = -223;
pub const PSEUDO_ENCODING_LAST_RECT: i32 = -224;
pub const PSEUDO_ENCODING_EXT_KEY_EVENT: i32 = -258;
pub const PSEUDO_ENCODING_DESKTOP_NAME: i32 = -307;
pub const PSEUDO_ENCODING_CONTINUOUS_UPDATES: i32 = -313;

// Client-to-Server Message IDs
pub const CLIENT_MSG_SET_PIXEL_FORMAT: u8 = 0;
pub const CLIENT_MSG_SET_ENCODINGS: u8 = 2;
pub const CLIENT_MSG_FRAMEBUFFER_UPDATE_REQ: u8 = 3;
pub const CLIENT_MSG_KEY_EVENT: u8 = 4;
pub const CLIENT_MSG_POINTER_EVENT: u8 = 5;
pub const CLIENT_MSG_CLIENT_CUT_TEXT: u8 = 6;
pub const CLIENT_MSG_ENABLE_CONTINUOUS_UPDATES: u8 = 150;

// Server-to-Client Message IDs
pub const SERVER_MSG_FRAMEBUFFER_UPDATE: u8 = 0;
pub const SERVER_MSG_SET_COLOUR_MAP_ENTRIES: u8 = 1;
pub const SERVER_MSG_BELL: u8 = 2;
pub const SERVER_MSG_SERVER_CUT_TEXT: u8 = 3;
pub const SERVER_MSG_END_OF_CONTINUOUS_UPDATES: u8 = 150;

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    SetPixelFormat(PixelFormat),
    SetEncodings(Vec<i32>),
    FramebufferUpdateRequest { incremental: bool, rect: Rect },
    KeyEvent { down: bool, key_sym: u32 },
    PointerEvent { button_mask: u8, x: u16, y: u16 },
    ClientCutText(String),
    EnableContinuousUpdates { enable: bool, rect: Rect },
}

impl ClientMessage {
    /// Attempt to parse a ClientMessage from a BytesMut buffer.
    /// Returns Ok(Some(msg)) on success and advances buffer.
    /// Returns Ok(None) if not enough bytes are available.
    pub fn parse(buf: &mut BytesMut) -> io::Result<Option<ClientMessage>> {
        if buf.is_empty() {
            return Ok(None);
        }

        let msg_type = buf[0];
        match msg_type {
            CLIENT_MSG_SET_PIXEL_FORMAT => {
                if buf.len() < 20 {
                    return Ok(None);
                }
                buf.advance(4); // Skip msg_type (1) + 3 padding bytes
                let mut fmt_bytes = [0u8; 16];
                fmt_bytes.copy_from_slice(&buf[..16]);
                buf.advance(16);
                let format = PixelFormat::from_bytes(&fmt_bytes);
                Ok(Some(ClientMessage::SetPixelFormat(format)))
            }
            CLIENT_MSG_SET_ENCODINGS => {
                if buf.len() < 4 {
                    return Ok(None);
                }
                let count = u16::from_be_bytes([buf[2], buf[3]]) as usize;
                let needed = 4 + count * 4;
                if buf.len() < needed {
                    return Ok(None);
                }
                buf.advance(4); // Skip msg_type (1) + pad (1) + count (2)
                let mut encodings = Vec::with_capacity(count);
                for _ in 0..count {
                    let enc = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
                    buf.advance(4);
                    encodings.push(enc);
                }
                Ok(Some(ClientMessage::SetEncodings(encodings)))
            }
            CLIENT_MSG_FRAMEBUFFER_UPDATE_REQ => {
                if buf.len() < 10 {
                    return Ok(None);
                }
                let incremental = buf[1] != 0;
                let x = u16::from_be_bytes([buf[2], buf[3]]);
                let y = u16::from_be_bytes([buf[4], buf[5]]);
                let width = u16::from_be_bytes([buf[6], buf[7]]);
                let height = u16::from_be_bytes([buf[8], buf[9]]);
                buf.advance(10);
                Ok(Some(ClientMessage::FramebufferUpdateRequest {
                    incremental,
                    rect: Rect::new(x, y, width, height),
                }))
            }
            CLIENT_MSG_KEY_EVENT => {
                if buf.len() < 8 {
                    return Ok(None);
                }
                let down = buf[1] != 0;
                let key_sym = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                buf.advance(8);
                Ok(Some(ClientMessage::KeyEvent { down, key_sym }))
            }
            CLIENT_MSG_POINTER_EVENT => {
                if buf.len() < 6 {
                    return Ok(None);
                }
                let button_mask = buf[1];
                let x = u16::from_be_bytes([buf[2], buf[3]]);
                let y = u16::from_be_bytes([buf[4], buf[5]]);
                buf.advance(6);
                Ok(Some(ClientMessage::PointerEvent { button_mask, x, y }))
            }
            CLIENT_MSG_CLIENT_CUT_TEXT => {
                if buf.len() < 8 {
                    return Ok(None);
                }
                let length = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
                let total_needed = 8 + length;
                if buf.len() < total_needed {
                    return Ok(None);
                }
                buf.advance(8);
                let text_bytes = buf.split_to(length);
                let text = String::from_utf8_lossy(&text_bytes).to_string();
                Ok(Some(ClientMessage::ClientCutText(text)))
            }
            CLIENT_MSG_ENABLE_CONTINUOUS_UPDATES => {
                if buf.len() < 10 {
                    return Ok(None);
                }
                let enable = buf[1] != 0;
                let x = u16::from_be_bytes([buf[2], buf[3]]);
                let y = u16::from_be_bytes([buf[4], buf[5]]);
                let width = u16::from_be_bytes([buf[6], buf[7]]);
                let height = u16::from_be_bytes([buf[8], buf[9]]);
                buf.advance(10);
                Ok(Some(ClientMessage::EnableContinuousUpdates {
                    enable,
                    rect: Rect::new(x, y, width, height),
                }))
            }
            unknown => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unknown client message type: {}", unknown),
            )),
        }
    }
}

/// An update rectangle header in FramebufferUpdate message
#[derive(Debug, Clone)]
pub struct UpdateRectHeader {
    pub rect: Rect,
    pub encoding: i32,
}

impl UpdateRectHeader {
    pub fn new(rect: Rect, encoding: i32) -> Self {
        Self { rect, encoding }
    }

    pub fn write_to(&self, buf: &mut BytesMut) {
        buf.put_u16(self.rect.x);
        buf.put_u16(self.rect.y);
        buf.put_u16(self.rect.width);
        buf.put_u16(self.rect.height);
        buf.put_i32(self.encoding);
    }
}

/// Server message builder helpers
pub struct ServerMessage;

impl ServerMessage {
    /// Build ServerInit payload
    pub fn server_init(
        width: u16,
        height: u16,
        format: &PixelFormat,
        desktop_name: &str,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(24 + desktop_name.len());
        buf.extend_from_slice(&width.to_be_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        buf.extend_from_slice(&format.to_bytes());
        buf.extend_from_slice(&(desktop_name.len() as u32).to_be_bytes());
        buf.extend_from_slice(desktop_name.as_bytes());
        buf
    }

    /// Build ServerCutText message
    pub fn server_cut_text(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes();
        let mut buf = Vec::with_capacity(8 + bytes.len());
        buf.push(SERVER_MSG_SERVER_CUT_TEXT);
        buf.extend_from_slice(&[0, 0, 0]); // 3 pad bytes
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
        buf
    }

    /// Build Bell message
    pub fn bell() -> [u8; 1] {
        [SERVER_MSG_BELL]
    }

    /// Build EndOfContinuousUpdates message
    pub fn end_of_continuous_updates() -> [u8; 1] {
        [SERVER_MSG_END_OF_CONTINUOUS_UPDATES]
    }
}
