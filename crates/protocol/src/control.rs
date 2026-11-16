//! Control channel: small reliable messages, length-prefixed on a QUIC stream.
//!
//! Frame layout: `u32 little-endian payload length` + postcard payload.
//!
//! Session handshake:
//!   viewer -> host  Hello { version, viewer_name, pin }
//!   host -> viewer  Welcome { host_name, stream }   or  Rejected { reason }
//!                   or AwaitingApproval, then later Welcome or Rejected
//! `pin` may be `None`: a viewer the host already authorized is recognised
//! by its TLS client certificate (see the `net` crate), never by anything
//! in `Hello`. Otherwise the host answers `Rejected { PinRequired }`, or,
//! when the person at the host can accept connections with a click,
//! `AwaitingApproval` first: the answer follows once they chose (or the
//! request expired).
//! then at any time:
//!   viewer -> host  RequestKeyframe
//!   viewer -> host  Input(event)            (ignored by view-only hosts)
//!   host -> viewer  Cursor(shape) / CursorVisible(bool)
//!   host -> viewer  InputAllowed(bool)      (remote control switched mid-session)
//!   viewer -> host  ReceptionReport(report) (every RECEPTION_REPORT_INTERVAL)
//!   host -> viewer  TargetBitrate { bps }   (the encoder's rate changed)
//!   viewer -> host  Nack(nack)              (video datagrams to send again)
//!   either          Clipboard(text)         (the text just copied, at most
//!                                            MAX_CLIPBOARD_BYTES)
//!   host -> viewer  MonitorList { .. }      (right after Welcome, and again
//!                                            whenever it changed)
//!   viewer -> host  SelectMonitor(index)    (also from view-only viewers)
//!   viewer -> host  SetQuality(quality)     (also from view-only viewers)
//!   host -> viewer  StreamChanged(stream)   (capture and encoder started
//!                                            over: another monitor, quality
//!                                            or resolution; same connection)
//!   either          Transfer(message)       (files pushed or pasted, see
//!                                            [`crate::transfer`])
//!   viewer -> host  SetAudio(on)            (also from view-only viewers;
//!                                            the sound itself travels as
//!                                            datagrams, see [`crate::audio`])
//!   viewer -> host  SecureAttention         (Ctrl+Alt+Del; unattended hosts)
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
use crate::recovery::Nack;
use crate::session::{MonitorEntry, Quality};
use crate::transfer::TransferMessage;

/// 2: remote control (Input, Cursor, CursorVisible; Welcome.input_allowed).
/// 3: InputAllowed; StreamInfo.encoder.
/// 4: Hello.pin optional (remembered viewers); Rejected carries a
///    [`RejectReason`]. The reasons added later (`Busy`,
///    `TooManyAttempts`) keep its text form, so they need no new version:
///    a viewer that does not know them shows their text as `Other`.
/// 5: ReceptionReport (viewer) and TargetBitrate (host): the host adapts
///    the encoder's bitrate to the network.
/// 6: Nack (viewer): lost video datagrams are sent again instead of
///    waiting for a keyframe.
/// 7: Clipboard (either side): copied text crosses to the other PC.
/// 8: MonitorList and StreamChanged (host), SelectMonitor and SetQuality
///    (viewer): the viewer picks the monitor and the quality mid-session,
///    and a resolution change on the host no longer ends it.
/// 9: AwaitingApproval (host): the person at the host is asked to accept or
///    refuse the viewer; `Welcome` or `Rejected` follows. Its refusals
///    (`Denied`, `Expired`, `TooManyRequests`) keep the text form.
/// 10: Transfer (either side): files cross between the two PCs, pushed or
///    copied and pasted, each on a unidirectional stream of its own.
/// 11: Audio datagrams (host, see [`crate::audio`]) and SetAudio (viewer):
///    the viewer hears the host and can mute it.
/// 12: SecureAttention (viewer): Ctrl+Alt+Del on a host shared with
///    unattended access. Its refusals (`PasswordRequired`, `WrongPassword`,
///    `NotAuthorized`) keep the text form.
/// 13: `TransferError` tells the known failures apart (too many transfers,
///    folders that cannot be created, no free name, planning that failed)
///    instead of sending them as English text. The refusals added to
///    [`RejectReason`] (`HostStopped`, `UnexpectedMessage`, `NoHello`,
///    `LeftBeforeHello`, `UnreadableHello`, `LeftBeforeWelcome`) keep the
///    text form.
pub const PROTOCOL_VERSION: u16 = 13;

/// Upper bound for one control message; protects against garbage lengths.
/// Sized for the largest cursor image (256x256 RGBA = 256 KiB) plus margin.
pub const MAX_CONTROL_MESSAGE: usize = 512 * 1024;

/// Longest text a [`ControlMessage::Clipboard`] carries, in UTF-8 bytes.
/// Both sides refuse longer text: the sender does not send it and the
/// receiver ignores it, so a peer cannot make the other one hold or paste
/// something huge.
pub const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

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
    /// The person at the host refused the viewer.
    Denied,
    /// Nobody at the host answered the viewer's request in time.
    Expired,
    /// The viewer's address asked for approval too often lately: the host
    /// refuses without asking the person at the host.
    TooManyRequests,
    /// The host is shared with unattended access and protected by a
    /// password, which the viewer did not send (in `Hello.pin`).
    PasswordRequired,
    WrongPassword,
    /// The host is shared with unattended access, which only devices
    /// authorized earlier may use.
    NotAuthorized,
    /// The host stopped sharing while the viewer was being let in.
    HostStopped,
    /// The first message was not a `Hello`.
    UnexpectedMessage,
    /// The viewer sent no `Hello` in time.
    NoHello,
    /// The viewer closed the connection before its `Hello`.
    LeftBeforeHello,
    /// The viewer's `Hello` could not be read; the text is why.
    UnreadableHello(String),
    /// The viewer closed the connection before the host's `Welcome` got
    /// to it; the text is why.
    LeftBeforeWelcome(String),
    /// Anything else, as text for people.
    Other(String),
}

const PIN_REQUIRED_TEXT: &str = "PIN required";
const WRONG_PIN_TEXT: &str = "wrong PIN";
const BUSY_TEXT: &str = "host busy: another viewer is connected";
const TOO_MANY_ATTEMPTS_TEXT: &str = "too many wrong PINs, try again later";
const DENIED_TEXT: &str = "the host refused the connection";
const EXPIRED_TEXT: &str = "nobody at the host answered in time";
const TOO_MANY_REQUESTS_TEXT: &str = "too many connection requests, try again later";
const PASSWORD_REQUIRED_TEXT: &str = "password required";
const WRONG_PASSWORD_TEXT: &str = "wrong password";
const NOT_AUTHORIZED_TEXT: &str = "this computer accepts only devices it already knows";
const HOST_STOPPED_TEXT: &str = "the host stopped sharing";
const UNEXPECTED_MESSAGE_TEXT: &str = "expected Hello";
const NO_HELLO_TEXT: &str = "viewer sent no Hello";
const LEFT_BEFORE_HELLO_TEXT: &str = "viewer left before Hello";
const CANNOT_STREAM_TEXT_PREFIX: &str = "host cannot stream:";
const UNREADABLE_HELLO_TEXT_PREFIX: &str = "unreadable Hello:";
const LEFT_BEFORE_WELCOME_TEXT_PREFIX: &str = "viewer left before Welcome:";
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
            Self::Denied => f.write_str(DENIED_TEXT),
            Self::Expired => f.write_str(EXPIRED_TEXT),
            Self::TooManyRequests => f.write_str(TOO_MANY_REQUESTS_TEXT),
            Self::PasswordRequired => f.write_str(PASSWORD_REQUIRED_TEXT),
            Self::WrongPassword => f.write_str(WRONG_PASSWORD_TEXT),
            Self::NotAuthorized => f.write_str(NOT_AUTHORIZED_TEXT),
            Self::HostStopped => f.write_str(HOST_STOPPED_TEXT),
            Self::UnexpectedMessage => f.write_str(UNEXPECTED_MESSAGE_TEXT),
            Self::NoHello => f.write_str(NO_HELLO_TEXT),
            Self::LeftBeforeHello => f.write_str(LEFT_BEFORE_HELLO_TEXT),
            Self::UnreadableHello(cause) => write!(f, "{UNREADABLE_HELLO_TEXT_PREFIX} {cause}"),
            Self::LeftBeforeWelcome(cause) => {
                write!(f, "{LEFT_BEFORE_WELCOME_TEXT_PREFIX} {cause}")
            }
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
            DENIED_TEXT => Self::Denied,
            EXPIRED_TEXT => Self::Expired,
            TOO_MANY_REQUESTS_TEXT => Self::TooManyRequests,
            PASSWORD_REQUIRED_TEXT => Self::PasswordRequired,
            WRONG_PASSWORD_TEXT => Self::WrongPassword,
            NOT_AUTHORIZED_TEXT => Self::NotAuthorized,
            HOST_STOPPED_TEXT => Self::HostStopped,
            UNEXPECTED_MESSAGE_TEXT => Self::UnexpectedMessage,
            NO_HELLO_TEXT => Self::NoHello,
            LEFT_BEFORE_HELLO_TEXT => Self::LeftBeforeHello,
            _ => Self::from_text_with_cause(text),
        }
    }
}

impl RejectReason {
    /// The reasons whose text is a fixed prefix followed by a cause (or
    /// the version text); anything else is [`Self::Other`].
    fn from_text_with_cause(text: String) -> Self {
        if let Some(host) = unsupported_version_host(&text) {
            return Self::UnsupportedVersion { host };
        }
        let cause_after = |prefix: &str| text.strip_prefix(prefix).map(|c| c.trim().to_string());
        if let Some(cause) = cause_after(CANNOT_STREAM_TEXT_PREFIX) {
            return Self::CannotStream(cause);
        }
        if let Some(cause) = cause_after(UNREADABLE_HELLO_TEXT_PREFIX) {
            return Self::UnreadableHello(cause);
        }
        if let Some(cause) = cause_after(LEFT_BEFORE_WELCOME_TEXT_PREFIX) {
            return Self::LeftBeforeWelcome(cause);
        }
        Self::Other(text)
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
        /// certificate. A host shared with unattended access reads the
        /// password of that access here instead of a PIN.
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
    /// Video datagrams the viewer missed: the host sends them again while
    /// it still has them (see [`crate::recovery`]).
    Nack(Nack),
    /// The text just copied on the sender's PC, for the other PC's
    /// clipboard. At most [`MAX_CLIPBOARD_BYTES`] bytes.
    Clipboard(String),
    /// The host's monitors (at most [`crate::session::MAX_MONITORS`]) and
    /// the position in that list of the one being shared.
    MonitorList {
        monitors: Vec<MonitorEntry>,
        current: u32,
    },
    /// Share the monitor at this position of the latest `MonitorList`.
    SelectMonitor(u32),
    SetQuality(Quality),
    /// The host restarted capture and encoder: the video from now on is
    /// this stream, beginning with a keyframe. Frame ids go on counting.
    StreamChanged(StreamInfo),
    /// The viewer is not authorized and the person at the host was asked to
    /// accept or refuse it: sent once, after `Hello`, and followed by
    /// `Welcome` or `Rejected` (`Denied`, or `Expired` when nobody answered).
    AwaitingApproval,
    /// A step of a file transfer (see [`crate::transfer`]).
    Transfer(TransferMessage),
    /// The viewer wants the host's sound (`true`, what every connection
    /// starts with) or mutes it (`false`): the host then stops capturing
    /// and sending it.
    SetAudio(bool),
    /// Ctrl+Alt+Del on the host (the secure attention sequence, which
    /// injected keys never trigger). Only a host shared with unattended
    /// access can honour it, and only while remote control is allowed.
    SecureAttention,
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
    use crate::recovery::Missing;
    use crate::session::{MAX_MONITOR_NAME, MAX_MONITORS};

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
            ControlMessage::Nack(Nack {
                frame_id: 300,
                missing: Missing::Packets(vec![1, 2]),
            }),
            ControlMessage::Nack(Nack {
                frame_id: 7,
                missing: Missing::Whole,
            }),
            ControlMessage::Clipboard("copiato \u{e8} \u{1F5A5}\r\nseconda riga".into()),
            ControlMessage::Clipboard(String::new()),
            ControlMessage::MonitorList {
                monitors: vec![
                    MonitorEntry::new("DISPLAY1", 1920, 1080, true),
                    MonitorEntry::new("DISPLAY2", 2560, 1440, false),
                ],
                current: 1,
            },
            ControlMessage::MonitorList {
                monitors: Vec::new(),
                current: 0,
            },
            ControlMessage::SelectMonitor(1),
            ControlMessage::SetQuality(Quality::Medium),
            ControlMessage::StreamChanged(StreamInfo {
                codec: VideoCodec::H264,
                width: 1280,
                height: 1024,
                fps: 30,
                encoder: "Test Encoder".into(),
            }),
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
            ControlMessage::Rejected {
                reason: RejectReason::Denied,
            },
            ControlMessage::Rejected {
                reason: RejectReason::Expired,
            },
            ControlMessage::Rejected {
                reason: RejectReason::TooManyRequests,
            },
            ControlMessage::AwaitingApproval,
            ControlMessage::Transfer(TransferMessage::Offer {
                id: crate::transfer::TransferId(3),
                entries: vec![
                    crate::transfer::FileEntry::folder("Foto"),
                    crate::transfer::FileEntry::file("Foto/mare.jpg", 1 << 40),
                ],
            }),
            ControlMessage::SetAudio(false),
            ControlMessage::SetAudio(true),
            ControlMessage::SecureAttention,
            ControlMessage::Rejected {
                reason: RejectReason::PasswordRequired,
            },
            ControlMessage::Rejected {
                reason: RejectReason::NotAuthorized,
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
            RejectReason::Denied,
            RejectReason::Expired,
            RejectReason::TooManyRequests,
            RejectReason::PasswordRequired,
            RejectReason::WrongPassword,
            RejectReason::NotAuthorized,
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
        assert_eq!(
            index(ControlMessage::Nack(Nack {
                frame_id: 0,
                missing: Missing::Whole
            })),
            11
        );
        assert_eq!(index(ControlMessage::Clipboard(String::new())), 12);
        assert_eq!(
            index(ControlMessage::MonitorList {
                monitors: Vec::new(),
                current: 0
            }),
            13
        );
        assert_eq!(index(ControlMessage::SelectMonitor(0)), 14);
        assert_eq!(index(ControlMessage::SetQuality(Quality::Automatic)), 15);
        assert_eq!(index(ControlMessage::StreamChanged(sample_stream())), 16);
        assert_eq!(index(ControlMessage::AwaitingApproval), 17);
        assert_eq!(
            index(ControlMessage::Transfer(TransferMessage::Cancel {
                id: crate::transfer::TransferId(0)
            })),
            18
        );
        assert_eq!(index(ControlMessage::SetAudio(true)), 19);
    }

    #[test]
    fn set_audio_layout_is_stable() {
        // Variant 19, then the flag as one byte: 0 = muted, 1 = on.
        assert_eq!(
            encode_frame(&ControlMessage::SetAudio(false)),
            frame_of(&[19, 0])
        );
        assert_eq!(
            encode_frame(&ControlMessage::SetAudio(true)),
            frame_of(&[19, 1])
        );
    }

    fn sample_stream() -> StreamInfo {
        StreamInfo {
            codec: VideoCodec::Hevc,
            width: 1280,
            height: 1024,
            fps: 30,
            encoder: "E".into(),
        }
    }

    #[test]
    fn monitor_list_layout_is_stable() {
        // Variant 13, the list (length, then per monitor: name as length +
        // bytes, width and height as varints, primary as a byte), then the
        // current position as a varint. 1920 = 0x80 0x0F, 1080 = 0xB8 0x08.
        let frame = encode_frame(&ControlMessage::MonitorList {
            monitors: vec![MonitorEntry::new("D1", 1920, 1080, true)],
            current: 0,
        });
        assert_eq!(
            frame,
            frame_of(&[13, 1, 2, b'D', b'1', 0x80, 0x0F, 0xB8, 0x08, 1, 0])
        );
    }

    #[test]
    fn select_monitor_and_set_quality_layouts_are_stable() {
        assert_eq!(
            encode_frame(&ControlMessage::SelectMonitor(300)),
            frame_of(&[14, 0xAC, 0x02])
        );
        // Quality: 0 = automatic, 1 = high, 2 = medium, 3 = low.
        for (quality, byte) in [
            (Quality::Automatic, 0),
            (Quality::High, 1),
            (Quality::Medium, 2),
            (Quality::Low, 3),
        ] {
            assert_eq!(
                encode_frame(&ControlMessage::SetQuality(quality)),
                frame_of(&[15, byte])
            );
        }
    }

    #[test]
    fn awaiting_approval_layout_is_stable() {
        // Variant 17, no fields.
        assert_eq!(
            encode_frame(&ControlMessage::AwaitingApproval),
            frame_of(&[17])
        );
    }

    #[test]
    fn secure_attention_layout_is_stable() {
        // Variant 20, no fields.
        assert_eq!(
            encode_frame(&ControlMessage::SecureAttention),
            frame_of(&[20])
        );
    }

    #[test]
    fn admission_refusals_are_encoded_as_text_and_read_back() {
        for (reason, text) in [
            (RejectReason::HostStopped, "the host stopped sharing"),
            (RejectReason::UnexpectedMessage, "expected Hello"),
            (RejectReason::NoHello, "viewer sent no Hello"),
            (RejectReason::LeftBeforeHello, "viewer left before Hello"),
            (
                RejectReason::UnreadableHello("bad frame".into()),
                "unreadable Hello: bad frame",
            ),
            (
                RejectReason::LeftBeforeWelcome("reset".into()),
                "viewer left before Welcome: reset",
            ),
        ] {
            assert_eq!(reason.to_string(), text);
            assert_eq!(RejectReason::from(text.to_string()), reason);
        }
    }

    #[test]
    fn unattended_refusals_are_encoded_as_text() {
        for (reason, text) in [
            (RejectReason::PasswordRequired, PASSWORD_REQUIRED_TEXT),
            (RejectReason::WrongPassword, WRONG_PASSWORD_TEXT),
            (RejectReason::NotAuthorized, NOT_AUTHORIZED_TEXT),
        ] {
            let mut expected = vec![2, u8::try_from(text.len()).unwrap()];
            expected.extend_from_slice(text.as_bytes());
            assert_eq!(
                encode_frame(&ControlMessage::Rejected { reason }),
                frame_of(&expected)
            );
        }
    }

    #[test]
    fn approval_refusals_are_encoded_as_text() {
        // Peers of other versions read them as plain text (`Other`), as
        // they do the reasons added after protocol 4.
        for (reason, text) in [
            (RejectReason::Denied, DENIED_TEXT),
            (RejectReason::Expired, EXPIRED_TEXT),
            (RejectReason::TooManyRequests, TOO_MANY_REQUESTS_TEXT),
        ] {
            let mut expected = vec![2, u8::try_from(text.len()).unwrap()];
            expected.extend_from_slice(text.as_bytes());
            assert_eq!(
                encode_frame(&ControlMessage::Rejected { reason }),
                frame_of(&expected)
            );
        }
    }

    #[test]
    fn stream_changed_layout_is_stable() {
        // Variant 16, then the StreamInfo laid out as in Welcome: codec
        // (0 = H.264, 1 = HEVC), width, height and fps as varints, the
        // encoder name. 1280 = 0x80 0x0A, 1024 = 0x80 0x08.
        let frame = encode_frame(&ControlMessage::StreamChanged(sample_stream()));
        assert_eq!(
            frame,
            frame_of(&[16, 1, 0x80, 0x0A, 0x80, 0x08, 30, 1, b'E'])
        );
    }

    #[test]
    fn the_longest_monitor_list_fits_in_one_frame() {
        let name = "\u{1F5A5}".repeat(MAX_MONITOR_NAME);
        let message = ControlMessage::MonitorList {
            monitors: vec![MonitorEntry::new(&name, u32::MAX, u32::MAX, true); MAX_MONITORS],
            current: u32::MAX,
        };
        let frame = encode_frame(&message);
        assert!(frame.len() - LENGTH_PREFIX <= MAX_CONTROL_MESSAGE);
        assert_eq!(decode_one(&frame).unwrap(), Some(message));
    }

    #[test]
    fn clipboard_layout_is_stable() {
        // Variant 12, then the text: length as a varint (3), UTF-8 bytes.
        let frame = encode_frame(&ControlMessage::Clipboard("a\u{e8}".into()));
        assert_eq!(frame, frame_of(&[12, 3, b'a', 0xC3, 0xA8]));
    }

    #[test]
    fn the_longest_clipboard_text_fits_in_one_frame() {
        let text = "x".repeat(MAX_CLIPBOARD_BYTES);
        let frame = encode_frame(&ControlMessage::Clipboard(text.clone()));
        assert!(frame.len() - LENGTH_PREFIX <= MAX_CONTROL_MESSAGE);
        assert_eq!(
            decode_one(&frame).unwrap(),
            Some(ControlMessage::Clipboard(text))
        );
    }

    #[test]
    fn nack_layout_is_stable() {
        // Variant 11, the frame id as a varint (300 = 0xAC 0x02), then the
        // `Missing` variant: 0 = whole frame, 1 = a list (length, indices
        // as varints: 200 = 0xC8 0x01).
        let packets = encode_frame(&ControlMessage::Nack(Nack {
            frame_id: 300,
            missing: Missing::Packets(vec![1, 200]),
        }));
        assert_eq!(packets, frame_of(&[11, 0xAC, 0x02, 1, 2, 1, 0xC8, 0x01]));
        let whole = encode_frame(&ControlMessage::Nack(Nack {
            frame_id: 7,
            missing: Missing::Whole,
        }));
        assert_eq!(whole, frame_of(&[11, 7, 0]));
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
