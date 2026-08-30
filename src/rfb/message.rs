use crate::display::framebuffer::{PixelFormat, Rect};
use bytes::{Buf, BytesMut};
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

/// An update rectangle header in FramebufferUpdate message (RFC 6143 Section 7.6.1)
/// Total size: exactly 12 bytes Big-Endian:
/// - x-position: 2 bytes (u16 BE)
/// - y-position: 2 bytes (u16 BE)
/// - width: 2 bytes (u16 BE)
/// - height: 2 bytes (u16 BE)
/// - encoding-type: 4 bytes (i32 BE)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateRectHeader {
    pub rect: Rect,
    pub encoding: i32,
}

impl UpdateRectHeader {
    pub const HEADER_SIZE: usize = 12;

    #[inline]
    pub fn new(rect: Rect, encoding: i32) -> Self {
        Self { rect, encoding }
    }

    #[inline]
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[0..2].copy_from_slice(&self.rect.x.to_be_bytes());
        buf[2..4].copy_from_slice(&self.rect.y.to_be_bytes());
        buf[4..6].copy_from_slice(&self.rect.width.to_be_bytes());
        buf[6..8].copy_from_slice(&self.rect.height.to_be_bytes());
        buf[8..12].copy_from_slice(&self.encoding.to_be_bytes());
        buf
    }

    #[inline]
    pub fn write_to(&self, buf: &mut BytesMut) {
        buf.extend_from_slice(&self.to_bytes());
    }

    pub fn parse(slice: &[u8]) -> Option<Self> {
        if slice.len() < 12 {
            return None;
        }
        let x = u16::from_be_bytes([slice[0], slice[1]]);
        let y = u16::from_be_bytes([slice[2], slice[3]]);
        let width = u16::from_be_bytes([slice[4], slice[5]]);
        let height = u16::from_be_bytes([slice[6], slice[7]]);
        let encoding = i32::from_be_bytes([slice[8], slice[9], slice[10], slice[11]]);
        Some(Self {
            rect: Rect::new(x, y, width, height),
            encoding,
        })
    }
}

/// Server message builder helpers
pub struct ServerMessage;

impl ServerMessage {
    /// Build ServerInit payload (RFC 6143 Section 7.3.2)
    pub fn server_init(
        width: u16,
        height: u16,
        format: &PixelFormat,
        desktop_name: &str,
    ) -> Vec<u8> {
        let name_bytes = desktop_name.as_bytes();
        let mut buf = Vec::with_capacity(24 + name_bytes.len());
        buf.extend_from_slice(&width.to_be_bytes());
        buf.extend_from_slice(&height.to_be_bytes());
        buf.extend_from_slice(&format.to_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(name_bytes);
        buf
    }

    /// Build ServerCutText message (RFC 6143 Section 7.6.4)
    pub fn server_cut_text(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes();
        let mut buf = Vec::with_capacity(8 + bytes.len());
        buf.push(SERVER_MSG_SERVER_CUT_TEXT);
        buf.extend_from_slice(&[0, 0, 0]); // 3 pad bytes
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
        buf
    }

    /// Build Bell message (RFC 6143 Section 7.6.3)
    pub fn bell() -> [u8; 1] {
        [SERVER_MSG_BELL]
    }

    /// Build EndOfContinuousUpdates message
    pub fn end_of_continuous_updates() -> [u8; 1] {
        [SERVER_MSG_END_OF_CONTINUOUS_UPDATES]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BufMut;

    #[test]
    fn test_update_rect_header_12_bytes_big_endian() {
        let header = UpdateRectHeader::new(Rect::new(10, 20, 64, 64), ENCODING_RAW);
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), 12);
        assert_eq!(&bytes[0..2], &10u16.to_be_bytes());
        assert_eq!(&bytes[2..4], &20u16.to_be_bytes());
        assert_eq!(&bytes[4..6], &64u16.to_be_bytes());
        assert_eq!(&bytes[6..8], &64u16.to_be_bytes());
        assert_eq!(&bytes[8..12], &0i32.to_be_bytes());

        let mut buf = BytesMut::new();
        header.write_to(&mut buf);
        assert_eq!(&buf[..], &bytes[..]);

        let parsed = UpdateRectHeader::parse(&bytes).expect("Failed to parse header");
        assert_eq!(parsed, header);
    }

    #[test]
    fn test_update_rect_header_pseudo_encodings() {
        // LastRect (-224)
        let last_rect = UpdateRectHeader::new(Rect::new(0, 0, 0, 0), PSEUDO_ENCODING_LAST_RECT);
        let bytes = last_rect.to_bytes();
        assert_eq!(bytes.len(), 12);
        assert_eq!(&bytes[8..12], &(-224i32).to_be_bytes());
        let parsed = UpdateRectHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.encoding, PSEUDO_ENCODING_LAST_RECT);

        // DesktopSize (-223)
        let dt_size = UpdateRectHeader::new(Rect::new(0, 0, 1920, 1080), PSEUDO_ENCODING_DESKTOP_SIZE);
        let bytes = dt_size.to_bytes();
        let parsed = UpdateRectHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.rect.width, 1920);
        assert_eq!(parsed.rect.height, 1080);
        assert_eq!(parsed.encoding, PSEUDO_ENCODING_DESKTOP_SIZE);
    }

    #[test]
    fn test_client_message_parse_set_encodings() {
        let mut buf = BytesMut::new();
        buf.put_u8(CLIENT_MSG_SET_ENCODINGS);
        buf.put_u8(0); // pad
        buf.put_u16(3); // count
        buf.put_i32(ENCODING_RAW);
        buf.put_i32(ENCODING_TIGHT);
        buf.put_i32(PSEUDO_ENCODING_LAST_RECT);

        let msg = ClientMessage::parse(&mut buf).unwrap().expect("parsed message");
        match msg {
            ClientMessage::SetEncodings(encs) => {
                assert_eq!(encs, vec![ENCODING_RAW, ENCODING_TIGHT, PSEUDO_ENCODING_LAST_RECT]);
            }
            _ => panic!("Unexpected message type"),
        }
    }

    #[test]
    fn test_client_message_parse_framebuffer_update_req() {
        let mut buf = BytesMut::new();
        buf.put_u8(CLIENT_MSG_FRAMEBUFFER_UPDATE_REQ);
        buf.put_u8(1); // incremental
        buf.put_u16(0); // x
        buf.put_u16(0); // y
        buf.put_u16(1920); // w
        buf.put_u16(1080); // h

        let msg = ClientMessage::parse(&mut buf).unwrap().expect("parsed message");
        match msg {
            ClientMessage::FramebufferUpdateRequest { incremental, rect } => {
                assert!(incremental);
                assert_eq!(rect, Rect::new(0, 0, 1920, 1080));
            }
            _ => panic!("Unexpected message type"),
        }
    }

    #[test]
    fn test_server_init_payload() {
        let fmt = PixelFormat::bgra32();
        let payload = ServerMessage::server_init(1920, 1080, &fmt, "TestDesktop");
        assert_eq!(&payload[0..2], &1920u16.to_be_bytes());
        assert_eq!(&payload[2..4], &1080u16.to_be_bytes());
        assert_eq!(&payload[4..20], &fmt.to_bytes()[..]);
        assert_eq!(&payload[20..24], &11u32.to_be_bytes());
        assert_eq!(&payload[24..], b"TestDesktop");
    }
}

