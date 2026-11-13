//! Control channel: small reliable messages, length-prefixed on a QUIC stream.
//!
//! Frame layout: `u32 little-endian payload length` + postcard payload.
//!
//! Session handshake:
//!   viewer -> host  Hello { version, viewer_name, pin }
//!   host -> viewer  Welcome { host_name, stream }   or  Rejected { reason }
//! then at any time:
//!   viewer -> host  RequestKeyframe
//!   viewer -> host  Input(event)            (ignored by view-only hosts)
//!   host -> viewer  Cursor(shape) / CursorVisible(bool)
//!   host -> viewer  InputAllowed(bool)      (remote control switched mid-session)
//!   either          Bye

use serde::{Deserialize, Serialize};

use crate::ProtocolError;
use crate::input::{CursorShape, InputEvent};

/// 2: remote control (Input, Cursor, CursorVisible; Welcome.input_allowed).
/// 3: InputAllowed; StreamInfo.encoder.
pub const PROTOCOL_VERSION: u16 = 3;

/// Upper bound for one control message; protects against garbage lengths.
/// Sized for the largest cursor image (256x256 RGBA = 256 KiB) plus margin.
pub const MAX_CONTROL_MESSAGE: usize = 512 * 1024;

/// Size of the little-endian length prefix in front of every payload.
const LENGTH_PREFIX: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Human-readable encoder name (e.g. "Intel Quick Sync H.264 Encoder
    /// MFT"), shown in the viewer's statistics.
    pub encoder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello {
        version: u16,
        viewer_name: String,
        pin: String,
    },
    Welcome {
        host_name: String,
        stream: StreamInfo,
        /// False when the host shares view-only: Input messages are ignored.
        input_allowed: bool,
    },
    Rejected {
        reason: String,
    },
    RequestKeyframe,
    Input(InputEvent),
    Cursor(CursorShape),
    /// The host pointer is hidden (e.g. a fullscreen video) or shown again.
    CursorVisible(bool),
    Bye,
    /// The host allowed or forbade remote mouse/keyboard during the session.
    InputAllowed(bool),
}

/// Serializes `msg` as one length-prefixed frame.
///
/// The caller is responsible for keeping messages below
/// [`MAX_CONTROL_MESSAGE`] (e.g. by bounding user-provided strings); the
/// receiving [`FrameDecoder`] rejects larger frames.
pub fn encode_frame(msg: &ControlMessage) -> Vec<u8> {
    let payload =
        postcard::to_allocvec(msg).expect("ControlMessage only contains postcard-supported types");
    let len = u32::try_from(payload.len()).expect("control message length fits in u32");
    let mut frame = Vec::with_capacity(LENGTH_PREFIX + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

/// Incremental decoder for a byte stream of length-prefixed frames. Bytes
/// can arrive split at arbitrary points.
///
/// Error semantics:
/// - An oversized length prefix ([`ProtocolError::TooLarge`]) means the stream
///   can no longer be trusted: the decoder keeps returning that error and the
///   caller should close the connection.
/// - An undecodable payload ([`ProtocolError::Decode`]) consumes only that
///   frame; since its length was valid, later messages still decode.
#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are
    /// needed, or an error for an oversized/undecodable frame.
    pub fn next_message(&mut self) -> Result<Option<ControlMessage>, ProtocolError> {
        let Some(prefix) = self.buffer.first_chunk::<LENGTH_PREFIX>() else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(*prefix) as usize;
        if len > MAX_CONTROL_MESSAGE {
            return Err(ProtocolError::TooLarge(len));
        }
        let frame_end = LENGTH_PREFIX + len;
        if self.buffer.len() < frame_end {
            return Ok(None);
        }
        let result = decode_payload(&self.buffer[LENGTH_PREFIX..frame_end]);
        self.buffer.drain(..frame_end);
        result.map(Some)
    }
}

/// Decodes exactly one message from `payload`; trailing bytes are an error
/// because they indicate a framing or version mismatch.
fn decode_payload(payload: &[u8]) -> Result<ControlMessage, ProtocolError> {
    let (msg, rest) = postcard::take_from_bytes::<ControlMessage>(payload)
        .map_err(|e| ProtocolError::Decode(e.to_string()))?;
    if !rest.is_empty() {
        return Err(ProtocolError::Decode(format!(
            "{} trailing bytes",
            rest.len()
        )));
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<ControlMessage> {
        vec![
            ControlMessage::Hello {
                version: PROTOCOL_VERSION,
                viewer_name: "laptop".into(),
                pin: "1234".into(),
            },
            ControlMessage::Welcome {
                host_name: "desktop".into(),
                stream: StreamInfo {
                    codec: VideoCodec::Hevc,
                    width: 2560,
                    height: 1440,
                    fps: 60,
                    encoder: "Test Encoder".into(),
                },
                input_allowed: true,
            },
            ControlMessage::Input(InputEvent::MouseMove { x: 0.25, y: 0.75 }),
            ControlMessage::Input(InputEvent::Key {
                scancode: 0x1E,
                extended: false,
                pressed: true,
            }),
            ControlMessage::Input(InputEvent::ReleaseAll),
            ControlMessage::Cursor(CursorShape {
                id: 7,
                width: 2,
                height: 1,
                hotspot_x: 1,
                hotspot_y: 0,
                rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
            }),
            ControlMessage::CursorVisible(false),
            ControlMessage::InputAllowed(true),
            ControlMessage::Rejected {
                reason: "bad pin".into(),
            },
            ControlMessage::RequestKeyframe,
            ControlMessage::Bye,
        ]
    }

    fn drain(decoder: &mut FrameDecoder) -> Vec<ControlMessage> {
        let mut out = Vec::new();
        while let Some(msg) = decoder.next_message().unwrap() {
            out.push(msg);
        }
        out
    }

    #[test]
    fn frame_starts_with_little_endian_length() {
        let frame = encode_frame(&ControlMessage::Bye);
        let len = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(len, frame.len() - 4);
    }

    #[test]
    fn roundtrip_every_message() {
        for msg in samples() {
            let mut decoder = FrameDecoder::new();
            decoder.push(&encode_frame(&msg));
            assert_eq!(decoder.next_message().unwrap(), Some(msg));
            assert_eq!(decoder.next_message().unwrap(), None);
        }
    }

    #[test]
    fn empty_decoder_needs_more_bytes() {
        assert_eq!(FrameDecoder::new().next_message().unwrap(), None);
    }

    #[test]
    fn bytes_split_at_every_offset() {
        let msg = samples().remove(1);
        let frame = encode_frame(&msg);
        for split in 0..=frame.len() {
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame[..split]);
            let first = decoder.next_message().unwrap();
            if split == frame.len() {
                assert_eq!(first, Some(msg.clone()));
                continue;
            }
            assert_eq!(first, None, "split at {split}");
            decoder.push(&frame[split..]);
            assert_eq!(decoder.next_message().unwrap(), Some(msg.clone()));
        }
    }

    #[test]
    fn byte_by_byte_stream_of_many_messages() {
        let stream: Vec<u8> = samples().iter().flat_map(encode_frame).collect();
        let mut decoder = FrameDecoder::new();
        let mut out = Vec::new();
        for byte in stream {
            decoder.push(&[byte]);
            out.extend(drain(&mut decoder));
        }
        assert_eq!(out, samples());
    }

    #[test]
    fn multiple_messages_in_one_push() {
        let stream: Vec<u8> = samples().iter().flat_map(encode_frame).collect();
        let mut decoder = FrameDecoder::new();
        decoder.push(&stream);
        assert_eq!(drain(&mut decoder), samples());
    }

    #[test]
    fn oversized_length_is_rejected_and_sticky() {
        let mut decoder = FrameDecoder::new();
        let len = (MAX_CONTROL_MESSAGE + 1) as u32;
        decoder.push(&len.to_le_bytes());
        assert_eq!(
            decoder.next_message(),
            Err(ProtocolError::TooLarge(MAX_CONTROL_MESSAGE + 1))
        );
        assert_eq!(
            decoder.next_message(),
            Err(ProtocolError::TooLarge(MAX_CONTROL_MESSAGE + 1))
        );
    }

    #[test]
    fn max_length_is_accepted_as_pending() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&(MAX_CONTROL_MESSAGE as u32).to_le_bytes());
        assert_eq!(decoder.next_message(), Ok(None));
    }

    #[test]
    fn undecodable_payload_is_skipped() {
        let mut decoder = FrameDecoder::new();
        // Variant index 200 does not exist.
        decoder.push(&[1, 0, 0, 0, 200]);
        decoder.push(&encode_frame(&ControlMessage::Bye));
        assert!(matches!(
            decoder.next_message(),
            Err(ProtocolError::Decode(_))
        ));
        assert_eq!(decoder.next_message().unwrap(), Some(ControlMessage::Bye));
    }

    #[test]
    fn trailing_bytes_in_payload_are_an_error() {
        let mut frame = encode_frame(&ControlMessage::Bye);
        frame.push(0);
        frame[..4].copy_from_slice(&2u32.to_le_bytes());
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_message(),
            Err(ProtocolError::Decode(_))
        ));
        assert_eq!(decoder.next_message().unwrap(), None);
    }
}
