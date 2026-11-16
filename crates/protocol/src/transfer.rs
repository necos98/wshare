//! File transfers between the two PCs: the control messages that propose,
//! accept, request and cancel them, and the header in front of every file
//! stream.
//!
//! Every file travels on a QUIC unidirectional stream of its own, opened by
//! the side that has the file: the [`StreamHeader`] (length-prefixed like a
//! control frame, at most [`MAX_STREAM_HEADER`] bytes), then the file's
//! bytes, then the end of the stream. Folders need no stream: the list of
//! entries already names them.
//!
//! Two ways for files to cross:
//! - pushed (drag and drop, "Invia file…"): the sender proposes the list
//!   with [`TransferMessage::Offer`], the receiver answers `Accept` or
//!   `Failed`, and after `Accept` the sender streams every file of the
//!   list; the receiver says `Completed` when all arrived.
//! - pulled (copy on one PC, paste on the other): the PC whose clipboard
//!   holds files announces them with `ClipboardFiles`; nothing is sent
//!   until someone pastes, then the other PC asks for each file with
//!   `Request` and the owner streams it.
//!
//! Either side may `Cancel` a transfer at any time (and reset or stop its
//! stream). Entry paths are relative, with `/` between folders, and are
//! untrusted: the receiver checks every one before using it.

use serde::{Deserialize, Serialize};

use crate::ProtocolError;

/// Unidirectional streams either side may have open at once: as many files
/// can travel at the same time.
pub const MAX_FILE_STREAMS: u32 = 4;

/// Most entries (files and folders) one list may hold.
pub const MAX_TRANSFER_ENTRIES: usize = 4096;

/// Longest relative path, in UTF-16 units: what the shell's file
/// descriptors (`FILEDESCRIPTORW`) hold, a pasted file being one of them.
pub const MAX_PATH_UNITS: usize = 259;

/// Most bytes the paths of one list may take together, so the list fits in
/// one control message (see `crate::control::MAX_CONTROL_MESSAGE`) with
/// room for the sizes and the message itself.
pub const MAX_ENTRY_LIST_BYTES: usize = 384 * 1024;

/// Upper bound of a stream header's payload.
pub const MAX_STREAM_HEADER: usize = 4096;

/// Separator of the folders of an entry path on the wire.
pub const PATH_SEPARATOR: char = '/';

/// Size of the little-endian length in front of a stream header.
pub const HEADER_LENGTH_PREFIX: usize = 4;

/// Names one transfer within a session. Each side numbers the transfers
/// it starts: the host with even numbers, the viewer with odd ones, so the
/// two never pick the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransferId(pub u32);

/// Names one announcement of clipboard files; a newer one replaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OfferId(pub u32);

/// One file or folder of a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the list's root, folders separated by [`PATH_SEPARATOR`].
    pub path: String,
    /// In bytes; 0 for a folder.
    pub size: u64,
    pub is_dir: bool,
}

impl FileEntry {
    pub fn file(path: impl Into<String>, size: u64) -> Self {
        Self {
            path: path.into(),
            size,
            is_dir: false,
        }
    }

    pub fn folder(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            size: 0,
            is_dir: true,
        }
    }
}

/// Whether `entries` fit in one message: few enough, with paths short
/// enough in all.
pub fn list_fits(entries: &[FileEntry]) -> bool {
    entries.len() <= MAX_TRANSFER_ENTRIES
        && entries.iter().map(|e| e.path.len()).sum::<usize>() <= MAX_ENTRY_LIST_BYTES
}

/// Why a transfer did not take place or stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferError {
    /// The receiver's settings or permissions do not allow it.
    NotAllowed,
    /// The list or a stream broke the rules (paths, sizes, counts).
    Invalid,
    /// The files are no longer offered (the clipboard changed) or cannot be
    /// read.
    Unavailable,
    /// Anything else (a full disk...), as text for people.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferMessage {
    /// The sender proposes to push these entries.
    Offer {
        id: TransferId,
        entries: Vec<FileEntry>,
    },
    /// The receiver takes the pushed entries: the files may come.
    Accept { id: TransferId },
    /// The transfer did not take place or stopped on the side that says so.
    Failed {
        id: TransferId,
        reason: TransferError,
    },
    /// Files were copied on the sender's PC: they can be pasted on the
    /// other one.
    ClipboardFiles {
        offer: OfferId,
        entries: Vec<FileEntry>,
    },
    /// Someone pastes: send entry `index` of the clipboard files `offer`,
    /// as transfer `id` (chosen by the side that asks).
    Request {
        offer: OfferId,
        index: u32,
        id: TransferId,
    },
    /// Stop the transfer: its stream is reset, partial files are deleted.
    Cancel { id: TransferId },
    /// Every file of the pushed transfer arrived.
    Completed { id: TransferId },
}

/// What opens every file stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamHeader {
    pub id: TransferId,
    /// Position of the entry in the list it comes from.
    pub index: u32,
    /// The entry as listed: the receiver checks it is the one it expects.
    pub entry: FileEntry,
}

/// `header` as it opens a stream: little-endian length, postcard payload.
pub fn encode_header(header: &StreamHeader) -> Vec<u8> {
    let payload =
        postcard::to_allocvec(header).expect("StreamHeader only contains postcard-supported types");
    let len = u32::try_from(payload.len()).expect("stream header length fits in u32");
    let mut frame = Vec::with_capacity(HEADER_LENGTH_PREFIX + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

/// The payload length announced by a header's prefix, if acceptable.
pub fn header_length(prefix: [u8; HEADER_LENGTH_PREFIX]) -> Result<usize, ProtocolError> {
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_STREAM_HEADER {
        return Err(ProtocolError::TooLarge(len));
    }
    Ok(len)
}

/// Decodes a header's payload; trailing bytes are an error.
pub fn decode_header(payload: &[u8]) -> Result<StreamHeader, ProtocolError> {
    let (header, rest) = postcard::take_from_bytes::<StreamHeader>(payload)
        .map_err(|e| ProtocolError::Decode(e.to_string()))?;
    if !rest.is_empty() {
        return Err(ProtocolError::Decode(format!(
            "{} trailing bytes",
            rest.len()
        )));
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> StreamHeader {
        StreamHeader {
            id: TransferId(7),
            index: 2,
            entry: FileEntry::file("Foto/mare è.jpg", 300),
        }
    }

    #[test]
    fn a_header_roundtrips() {
        let frame = encode_header(&header());
        let prefix: [u8; 4] = frame[..4].try_into().unwrap();
        let len = header_length(prefix).unwrap();
        assert_eq!(len, frame.len() - 4);
        assert_eq!(decode_header(&frame[4..]).unwrap(), header());
    }

    #[test]
    fn the_header_layout_is_stable() {
        // Id and index as varints, then the entry: path (length + UTF-8),
        // size as a varint (300 = 0xAC 0x02), folder flag.
        let frame = encode_header(&StreamHeader {
            id: TransferId(7),
            index: 2,
            entry: FileEntry::file("a/b", 300),
        });
        assert_eq!(
            frame,
            [9, 0, 0, 0, 7, 2, 3, b'a', b'/', b'b', 0xAC, 0x02, 0]
        );
    }

    #[test]
    fn an_oversized_header_length_is_refused() {
        let len = (MAX_STREAM_HEADER as u32 + 1).to_le_bytes();
        assert_eq!(
            header_length(len),
            Err(ProtocolError::TooLarge(MAX_STREAM_HEADER + 1))
        );
        assert_eq!(
            header_length((MAX_STREAM_HEADER as u32).to_le_bytes()),
            Ok(MAX_STREAM_HEADER)
        );
    }

    #[test]
    fn garbage_and_trailing_bytes_are_not_headers() {
        assert!(decode_header(&[]).is_err());
        let mut frame = encode_header(&header());
        frame.push(0);
        assert!(matches!(
            decode_header(&frame[4..]),
            Err(ProtocolError::Decode(_))
        ));
    }

    #[test]
    fn the_longest_header_fits() {
        // The longest path in UTF-16 units, each unit 3 UTF-8 bytes.
        let path = "\u{4E2D}".repeat(MAX_PATH_UNITS);
        let frame = encode_header(&StreamHeader {
            id: TransferId(u32::MAX),
            index: u32::MAX,
            entry: FileEntry::file(path, u64::MAX),
        });
        assert!(frame.len() - HEADER_LENGTH_PREFIX <= MAX_STREAM_HEADER);
    }

    mod layouts {
        //! The transfer messages as they travel inside a control frame:
        //! variant 18 of `ControlMessage`, then the `TransferMessage`.

        use super::super::*;
        use crate::control::{ControlMessage, FrameDecoder, MAX_CONTROL_MESSAGE, encode_frame};

        /// The payload of the control frame carrying `message`.
        fn payload(message: TransferMessage) -> Vec<u8> {
            encode_frame(&ControlMessage::Transfer(message))[4..].to_vec()
        }

        #[test]
        fn offer() {
            // Variant 0, id, entry count, then each entry.
            assert_eq!(
                payload(TransferMessage::Offer {
                    id: TransferId(3),
                    entries: vec![FileEntry::folder("d"), FileEntry::file("d/f", 5)],
                }),
                [18, 0, 3, 2, 1, b'd', 0, 1, 3, b'd', b'/', b'f', 5, 0]
            );
        }

        #[test]
        fn accept_failed_cancel_completed() {
            let id = TransferId(9);
            assert_eq!(payload(TransferMessage::Accept { id }), [18, 1, 9]);
            // Reasons: 0 not allowed, 1 invalid, 2 unavailable, 3 text.
            for (reason, bytes) in [
                (TransferError::NotAllowed, vec![0]),
                (TransferError::Invalid, vec![1]),
                (TransferError::Unavailable, vec![2]),
                (TransferError::Failed("no".into()), vec![3, 2, b'n', b'o']),
            ] {
                let mut expected = vec![18, 2, 9];
                expected.extend(bytes);
                assert_eq!(payload(TransferMessage::Failed { id, reason }), expected);
            }
            assert_eq!(payload(TransferMessage::Cancel { id }), [18, 5, 9]);
            assert_eq!(payload(TransferMessage::Completed { id }), [18, 6, 9]);
        }

        #[test]
        fn clipboard_files_and_request() {
            assert_eq!(
                payload(TransferMessage::ClipboardFiles {
                    offer: OfferId(4),
                    entries: vec![FileEntry::file("a", 300)],
                }),
                [18, 3, 4, 1, 1, b'a', 0xAC, 0x02, 0]
            );
            assert_eq!(
                payload(TransferMessage::Request {
                    offer: OfferId(4),
                    index: 1,
                    id: TransferId(6),
                }),
                [18, 4, 4, 1, 6]
            );
        }

        #[test]
        fn the_largest_list_fits_in_one_control_message() {
            // As many entries as allowed, each with the largest size: the
            // longest paths take the byte budget, one-byte paths the rest.
            let path = "x".repeat(MAX_PATH_UNITS);
            let long = (MAX_ENTRY_LIST_BYTES - MAX_TRANSFER_ENTRIES) / (path.len() - 1);
            let mut entries = vec![FileEntry::file(path, u64::MAX); long];
            entries.resize(MAX_TRANSFER_ENTRIES, FileEntry::file("y", u64::MAX));
            assert!(list_fits(&entries));
            let message = ControlMessage::Transfer(TransferMessage::ClipboardFiles {
                offer: OfferId(u32::MAX),
                entries,
            });
            let frame = encode_frame(&message);
            assert!(frame.len() - 4 <= MAX_CONTROL_MESSAGE);
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame);
            assert_eq!(decoder.next_message().unwrap(), Some(message));
        }
    }

    #[test]
    fn lists_are_bounded_by_count_and_bytes() {
        let small = vec![FileEntry::file("a", 1); MAX_TRANSFER_ENTRIES];
        assert!(list_fits(&small));
        let too_many = vec![FileEntry::file("a", 1); MAX_TRANSFER_ENTRIES + 1];
        assert!(!list_fits(&too_many));
        let long = "x".repeat(MAX_ENTRY_LIST_BYTES / 2 + 1);
        assert!(!list_fits(&[
            FileEntry::file(long.clone(), 1),
            FileEntry::file(long, 1)
        ]));
        assert!(list_fits(&[]));
    }
}
