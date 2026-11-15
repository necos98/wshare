//! Control channel: small reliable messages, length-prefixed on a QUIC stream.
//!
//! Frame layout: `u32 little-endian payload length` + postcard payload.
//!
//! Session handshake:
//!   viewer -> host  Hello { version, viewer_name, pin }
//!   host -> viewer  Welcome { host_name, stream }   or  Rejected { reason }
//! `pin` may be `None`: a viewer the host already authorized is recognised
//! by its TLS client certificate (see the `net` crate), never by anything
//! in `Hello`. Otherwise the host answers `Rejected { PinRequired }`.
//! then at any time:
//!   viewer -> host  RequestKeyframe
//!   viewer -> host  Input(event)            (ignored by view-only hosts)
//!   host -> viewer  Cursor(shape) / CursorVisible(bool)
//!   host -> viewer  InputAllowed(bool)      (remote control switched mid-session)
//!   viewer -> host  ReceptionReport(report) (every RECEPTION_REPORT_INTERVAL)
//!   host -> viewer  TargetBitrate { bps }   (the encoder's rate changed)
//!   either          Bye
//!
//! Compatibility: peers of different protocol versions must still understand
//! each other's first messages, since that is how a mismatch is reported.
//! Hence `Hello` keeps its variant index (0) and starts with the `u16`
//! `version` in every version, and `Rejected` keeps its index (2) and its
//! text form (see [`RejectReason`]): a host that cannot decode a `Hello`
//! reads the version from that prefix and answers `Rejected` (see
//! [`ProtocolError::IncompatibleHello`]). Anything after the prefix may
//! change with [`PROTOCOL_VERSION`].

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ProtocolError;
use crate::feedback::ReceptionReport;
use crate::input::{CursorShape, InputEvent};

/// 2: remote control (Input, Cursor, CursorVisible; Welcome.input_allowed).
/// 3: InputAllowed; StreamInfo.encoder.
/// 4: Hello.pin optional (remembered viewers); Rejected carries a
///    [`RejectReason`]. The reasons added later (`Busy`,
///    `TooManyAttempts`) keep its text form, so they need no new version:
///    a viewer that does not know them shows their text as `Other`.
/// 5: ReceptionReport (viewer) and TargetBitrate (host): the host adapts
///    the encoder's bitrate to the network.
pub const PROTOCOL_VERSION: u16 = 5;

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

/// Why the host refused a viewer.
///
/// On the wire this is the plain text of [`fmt::Display`], the same string
/// field protocol 3 used for `Rejected`: viewers of that version still
/// decode the message and show a readable reason (e.g. "version"), and
/// texts from such hosts map back to the matching variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum RejectReason {
    /// The viewer is not authorized yet and sent no PIN.
    PinRequired,
    WrongPin,
    /// The viewer speaks another protocol version; `host` is the host's.
    UnsupportedVersion {
        host: u16,
    },
    /// The host could not start streaming its screen; the text is why.
    CannotStream(String),
    /// Another viewer is already connected (who it is stays undisclosed).
    Busy,
    /// Too many wrong PINs came from the viewer's address lately: the host
    /// refuses without checking the PIN until the pause is over.
    TooManyAttempts,
    /// Anything else, as text for people.
    Other(String),
}

const PIN_REQUIRED_TEXT: &str = "PIN required";
const WRONG_PIN_TEXT: &str = "wrong PIN";
const BUSY_TEXT: &str = "host busy: another viewer is connected";
const TOO_MANY_ATTEMPTS_TEXT: &str = "too many wrong PINs, try again later";
const CANNOT_STREAM_TEXT_PREFIX: &str = "host cannot stream:";
/// Protocol 3 hosts wrote "protocol version V not supported (host uses H)";
/// both forms start and end the same way.
const VERSION_TEXT_PREFIX: &str = "protocol version";
const VERSION_TEXT_HOST: &str = "(host uses ";

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PinRequired => f.write_str(PIN_REQUIRED_TEXT),
            Self::WrongPin => f.write_str(WRONG_PIN_TEXT),
            Self::UnsupportedVersion { host } => {
                write!(
                    f,
                    "{VERSION_TEXT_PREFIX} not supported {VERSION_TEXT_HOST}{host})"
                )
            }
            Self::CannotStream(cause) => write!(f, "{CANNOT_STREAM_TEXT_PREFIX} {cause}"),
            Self::Busy => f.write_str(BUSY_TEXT),
            Self::TooManyAttempts => f.write_str(TOO_MANY_ATTEMPTS_TEXT),
            Self::Other(text) => f.write_str(text),
        }
    }
}

impl From<RejectReason> for String {
    fn from(reason: RejectReason) -> Self {
        reason.to_string()
    }
}

impl From<String> for RejectReason {
    fn from(text: String) -> Self {
        match text.as_str() {
            PIN_REQUIRED_TEXT => Self::PinRequired,
            WRONG_PIN_TEXT => Self::WrongPin,
            BUSY_TEXT => Self::Busy,
            TOO_MANY_ATTEMPTS_TEXT => Self::TooManyAttempts,
            _ => match unsupported_version_host(&text) {
                Some(host) => Self::UnsupportedVersion { host },
                None => match text.strip_prefix(CANNOT_STREAM_TEXT_PREFIX) {
                    Some(cause) => Self::CannotStream(cause.trim().to_string()),
                    None => Self::Other(text),
                },
            },
        }
    }
}

/// The host version in a version-mismatch text, if `text` is one.
fn unsupported_version_host(text: &str) -> Option<u16> {
    let rest = text.strip_prefix(VERSION_TEXT_PREFIX)?;
    let (_, host) = rest.rsplit_once(VERSION_TEXT_HOST)?;
    host.strip_suffix(')')?.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello {
        version: u16,
        viewer_name: String,
        /// `None` for a viewer that expects to be recognised by its client
        /// certificate.
        pin: Option<String>,
    },
    Welcome {
        host_name: String,
        stream: StreamInfo,
        /// False when the host shares view-only: Input messages are ignored.
        input_allowed: bool,
    },
    Rejected {
        reason: RejectReason,
    },
    RequestKeyframe,
    Input(InputEvent),
    Cursor(CursorShape),
    /// The host pointer is hidden (e.g. a fullscreen video) or shown again.
    CursorVisible(bool),
    Bye,
    /// The host allowed or forbade remote mouse/keyboard during the session.
    InputAllowed(bool),
    /// How the video fared since the previous report (sent every
    /// [`crate::feedback::RECEPTION_REPORT_INTERVAL`]).
    ReceptionReport(ReceptionReport),
    /// The bitrate the host's encoder now aims at, in bit/s: sent when
    /// streaming starts and whenever the host adapts it to the network.
    TargetBitrate {
        bps: u32,
    },
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

/// The start of [`ControlMessage::Hello`] that every protocol version
/// shares: the variant index (0) and the `u16` version. Reading only this
/// prefix tells which version a `Hello` we cannot decode belongs to.
#[derive(Deserialize)]
enum HelloPrefix {
    Hello { version: u16 },
}

/// Version announced by the `Hello` in `payload`, when its first bytes are
/// one (whatever follows them).
fn hello_version(payload: &[u8]) -> Option<u16> {
    match postcard::take_from_bytes::<HelloPrefix>(payload) {
        Ok((HelloPrefix::Hello { version }, _)) => Some(version),
        Err(_) => None,
    }
}

/// Decodes exactly one message from `payload`; trailing bytes are an error
/// because they indicate a framing or version mismatch. A `Hello` of
/// another protocol version, whose later fields may have another layout,
/// is reported as [`ProtocolError::IncompatibleHello`].
fn decode_payload(payload: &[u8]) -> Result<ControlMessage, ProtocolError> {
    decode_exact(payload).map_err(|error| match hello_version(payload) {
        Some(version) if version != PROTOCOL_VERSION => ProtocolError::IncompatibleHello(version),
        _ => error,
    })
}

fn decode_exact(payload: &[u8]) -> Result<ControlMessage, ProtocolError> {
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
                pin: Some("1234".into()),
            },
            ControlMessage::Hello {
                version: PROTOCOL_VERSION,
                viewer_name: "laptop".into(),
                pin: None,
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
            ControlMessage::ReceptionReport(ReceptionReport {
                frames_lost: 3,
                frames_late: 70_000,
            }),
            ControlMessage::TargetBitrate { bps: 20_000_000 },
            ControlMessage::Rejected {
                reason: RejectReason::PinRequired,
            },
            ControlMessage::Rejected {
                reason: RejectReason::UnsupportedVersion { host: 4 },
            },
            ControlMessage::Rejected {
                reason: RejectReason::CannotStream("no encoder".into()),
            },
            ControlMessage::Rejected {
                reason: RejectReason::Busy,
            },
            ControlMessage::Rejected {
                reason: RejectReason::TooManyAttempts,
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
    fn reject_reasons_roundtrip_through_their_text() {
        for reason in [
            RejectReason::PinRequired,
            RejectReason::WrongPin,
            RejectReason::UnsupportedVersion { host: 4 },
            RejectReason::CannotStream("no encoder".into()),
            RejectReason::Busy,
            RejectReason::TooManyAttempts,
            RejectReason::Other("busy".into()),
        ] {
            assert_eq!(RejectReason::from(reason.to_string()), reason);
        }
    }

    #[test]
    fn rejected_is_encoded_as_text_like_protocol_3() {
        let frame = encode_frame(&ControlMessage::Rejected {
            reason: RejectReason::WrongPin,
        });
        // Variant index 2, then a postcard string: length + bytes.
        let mut expected = vec![2, 9];
        expected.extend_from_slice(b"wrong PIN");
        assert_eq!(&frame[LENGTH_PREFIX..], expected);
    }

    #[test]
    fn protocol_3_texts_map_to_reasons() {
        assert_eq!(
            RejectReason::from("protocol version 4 not supported (host uses 3)".to_string()),
            RejectReason::UnsupportedVersion { host: 3 }
        );
        assert_eq!(
            RejectReason::from("wrong PIN".to_string()),
            RejectReason::WrongPin
        );
        assert_eq!(
            RejectReason::from("protocol version (host uses x)".to_string()),
            RejectReason::Other("protocol version (host uses x)".into())
        );
    }

    #[test]
    fn empty_decoder_needs_more_bytes() {
        assert_eq!(FrameDecoder::new().next_message().unwrap(), None);
    }

    #[test]
    fn bytes_split_at_every_offset() {
        let msg = samples().remove(2); // Welcome
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

    /// The frame of a message: little-endian length, then `payload`.
    fn frame_of(payload: &[u8]) -> Vec<u8> {
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(payload);
        frame
    }

    fn decode_one(frame: &[u8]) -> Result<Option<ControlMessage>, ProtocolError> {
        let mut decoder = FrameDecoder::new();
        decoder.push(frame);
        decoder.next_message()
    }

    #[test]
    fn hello_layout_is_stable() {
        // Variant 0, version (varint), name (length + bytes), then the PIN
        // as an option: 0 = none, 1 followed by a string.
        let with_pin = encode_frame(&ControlMessage::Hello {
            version: 4,
            viewer_name: "pc".into(),
            pin: Some("1234".into()),
        });
        assert_eq!(
            with_pin,
            frame_of(&[0, 4, 2, b'p', b'c', 1, 4, b'1', b'2', b'3', b'4'])
        );
        let without_pin = encode_frame(&ControlMessage::Hello {
            version: 4,
            viewer_name: "pc".into(),
            pin: None,
        });
        assert_eq!(without_pin, frame_of(&[0, 4, 2, b'p', b'c', 0]));
    }

    #[test]
    fn hello_version_is_a_varint_after_the_variant_index() {
        let frame = encode_frame(&ControlMessage::Hello {
            version: 300,
            viewer_name: String::new(),
            pin: None,
        });
        // 300 = 0b1_0010_1100 -> 0xAC 0x02.
        assert_eq!(&frame[LENGTH_PREFIX..], [0, 0xAC, 0x02, 0, 0]);
    }

    #[test]
    fn rejected_layout_is_stable() {
        for (reason, text) in [
            (RejectReason::PinRequired, "PIN required"),
            (RejectReason::WrongPin, "wrong PIN"),
            (
                RejectReason::UnsupportedVersion { host: 4 },
                "protocol version not supported (host uses 4)",
            ),
            (
                RejectReason::CannotStream("no encoder".into()),
                "host cannot stream: no encoder",
            ),
            (RejectReason::Busy, "host busy: another viewer is connected"),
            (
                RejectReason::TooManyAttempts,
                "too many wrong PINs, try again later",
            ),
            (RejectReason::Other("busy".into()), "busy"),
        ] {
            let mut payload = vec![2, text.len() as u8];
            payload.extend_from_slice(text.as_bytes());
            assert_eq!(
                encode_frame(&ControlMessage::Rejected { reason }),
                frame_of(&payload)
            );
        }
    }

    #[test]
    fn control_message_variant_indexes_are_stable() {
        let index = |msg: ControlMessage| encode_frame(&msg)[LENGTH_PREFIX];
        assert_eq!(index(samples().remove(0)), 0, "Hello");
        assert_eq!(index(samples().remove(2)), 1, "Welcome");
        assert_eq!(
            index(ControlMessage::Rejected {
                reason: RejectReason::WrongPin
            }),
            2
        );
        assert_eq!(index(ControlMessage::RequestKeyframe), 3);
        assert_eq!(index(ControlMessage::Bye), 7);
        assert_eq!(index(ControlMessage::InputAllowed(true)), 8);
        assert_eq!(
            index(ControlMessage::ReceptionReport(ReceptionReport::default())),
            9
        );
        assert_eq!(index(ControlMessage::TargetBitrate { bps: 0 }), 10);
    }

    #[test]
    fn reception_report_layout_is_stable() {
        // Variant 9, then both counts as varints: 3, and 300 = 0xAC 0x02.
        let frame = encode_frame(&ControlMessage::ReceptionReport(ReceptionReport {
            frames_lost: 3,
            frames_late: 300,
        }));
        assert_eq!(frame, frame_of(&[9, 3, 0xAC, 0x02]));
    }

    #[test]
    fn target_bitrate_layout_is_stable() {
        // Variant 10, then the rate as a varint: 20 000 000 = 0x01312D00,
        // seven bits at a time from the lowest.
        let frame = encode_frame(&ControlMessage::TargetBitrate { bps: 20_000_000 });
        assert_eq!(frame, frame_of(&[10, 0x80, 0xDA, 0xC4, 0x09]));
    }

    #[test]
    fn hello_of_protocol_3_is_reported_with_its_version() {
        // Protocol 3: version 3, name, then the PIN as a plain string.
        let payload = [0, 3, 2, b'p', b'c', 6, b'1', b'2', b'3', b'4', b'5', b'6'];
        assert_eq!(
            decode_one(&frame_of(&payload)),
            Err(ProtocolError::IncompatibleHello(3))
        );
    }

    #[test]
    fn hello_of_protocol_3_that_happens_to_decode_keeps_its_version() {
        // An empty PIN reads as "no PIN" in the current layout.
        let payload = [0, 3, 2, b'p', b'c', 0];
        assert_eq!(
            decode_one(&frame_of(&payload)),
            Ok(Some(ControlMessage::Hello {
                version: 3,
                viewer_name: "pc".into(),
                pin: None,
            }))
        );
    }

    #[test]
    fn hello_of_the_current_version_with_a_bad_body_is_a_plain_decode_error() {
        let payload = [0, PROTOCOL_VERSION as u8, 2, b'p', b'c', 9];
        assert!(matches!(
            decode_one(&frame_of(&payload)),
            Err(ProtocolError::Decode(_))
        ));
    }

    #[test]
    fn a_hello_cut_before_its_version_is_a_plain_decode_error() {
        assert!(matches!(
            decode_one(&frame_of(&[0])),
            Err(ProtocolError::Decode(_))
        ));
    }

    #[test]
    fn garbage_that_is_not_a_hello_is_a_plain_decode_error() {
        assert!(matches!(
            decode_one(&frame_of(&[200, 3, 1])),
            Err(ProtocolError::Decode(_))
        ));
    }
}
