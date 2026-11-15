//! Video datagrams.
//!
//! Each encoded frame is split into datagrams. Header (little-endian,
//! `HEADER_LEN` bytes) followed by payload:
//!
//! | off | size | field                                  |
//! |-----|------|----------------------------------------|
//! | 0   | 1    | version (= `VIDEO_VERSION`)            |
//! | 1   | 1    | flags (bit 0 = keyframe)               |
//! | 2   | 4    | frame_id (increments by 1 per frame)   |
//! | 6   | 2    | packet_index                           |
//! | 8   | 2    | packet_count (>= 1)                    |
//! | 10  | 8    | timestamp in 100 ns units (i64)        |
//!
//! Frame ids are u32 and never wrap in practice (2+ years at 60 fps).
//!
//! A datagram sent again after a [`crate::recovery::Nack`] is a byte-for-byte
//! copy of the original: the [`Reassembler`] treats it like any late
//! datagram, so retransmission needs nothing in this format.

mod reassembly;

use std::time::{Duration, Instant};

use bytes::{BufMut, Bytes, BytesMut};

pub use reassembly::{Gap, Reassembler, ReassemblyEvent};

use crate::ProtocolError;

pub const VIDEO_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 18;

/// Flag bit marking a packet that belongs to a keyframe.
const FLAG_KEYFRAME: u8 = 0b0000_0001;

/// Upper bound on never-seen frame ids reported individually as `Lost` for a
/// single gap. Ids are contiguous within a session, so real gaps are small;
/// the cap only protects against absurd ids producing huge event lists.
pub const MAX_REPORTED_GAP: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub keyframe: bool,
    pub frame_id: u32,
    pub packet_index: u16,
    pub packet_count: u16,
    pub timestamp: i64,
}

impl PacketHeader {
    /// Appends the `HEADER_LEN`-byte encoding of this header to `out`.
    pub fn write(&self, out: &mut impl BufMut) {
        let flags = if self.keyframe { FLAG_KEYFRAME } else { 0 };
        out.put_u8(VIDEO_VERSION);
        out.put_u8(flags);
        out.put_u32_le(self.frame_id);
        out.put_u16_le(self.packet_index);
        out.put_u16_le(self.packet_count);
        out.put_i64_le(self.timestamp);
    }

    /// Splits a datagram into its header and payload.
    ///
    /// Rejects datagrams shorter than the header, with an unknown version or
    /// with `packet_count == 0`. Unknown flag bits are ignored (forward
    /// compatible). `packet_index >= packet_count` is *not* rejected here:
    /// per the [`Reassembler`] policy such packets are silently ignored.
    pub fn parse(datagram: &[u8]) -> Result<(PacketHeader, &[u8]), ProtocolError> {
        let Some((header, payload)) = datagram.split_first_chunk::<HEADER_LEN>() else {
            return Err(ProtocolError::Truncated(datagram.len()));
        };
        if header[0] != VIDEO_VERSION {
            return Err(ProtocolError::BadVersion(header[0]));
        }
        let parsed = PacketHeader {
            keyframe: header[1] & FLAG_KEYFRAME != 0,
            frame_id: u32::from_le_bytes(sub(header, 2)),
            packet_index: u16::from_le_bytes(sub(header, 6)),
            packet_count: u16::from_le_bytes(sub(header, 8)),
            timestamp: i64::from_le_bytes(sub(header, 10)),
        };
        if parsed.packet_count == 0 {
            return Err(ProtocolError::Malformed("packet_count is zero"));
        }
        Ok((parsed, payload))
    }
}

/// Copies `N` bytes starting at `offset` (bounds are guaranteed by callers).
fn sub<const N: usize>(bytes: &[u8; HEADER_LEN], offset: usize) -> [u8; N] {
    bytes[offset..offset + N]
        .try_into()
        .expect("offset within header")
}

/// A fully reassembled encoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReassembledFrame {
    pub frame_id: u32,
    pub timestamp: i64,
    pub keyframe: bool,
    pub data: Bytes,
}

/// Splits encoded frames into datagrams.
///
/// Frame ids start at 0 and increase by one for every successfully
/// packetized frame.
pub struct Packetizer {
    next_frame_id: u32,
}

impl Packetizer {
    pub fn new() -> Self {
        Self { next_frame_id: 0 }
    }

    /// The id the next successfully packetized frame gets.
    pub fn next_frame_id(&self) -> u32 {
        self.next_frame_id
    }

    /// Splits one frame into datagrams no larger than `max_datagram_size`
    /// (header included), assigning the next frame id. Panics if
    /// `max_datagram_size <= HEADER_LEN`. An empty frame still yields one
    /// datagram. Errors if the frame would need more than u16::MAX packets
    /// (no frame id is consumed in that case).
    pub fn packetize(
        &mut self,
        data: &[u8],
        keyframe: bool,
        timestamp: i64,
        max_datagram_size: usize,
    ) -> Result<Vec<Bytes>, ProtocolError> {
        assert!(
            max_datagram_size > HEADER_LEN,
            "max_datagram_size ({max_datagram_size}) must exceed the {HEADER_LEN}-byte header"
        );
        let payload_size = max_datagram_size - HEADER_LEN;
        let chunk_count = data.len().div_ceil(payload_size).max(1);
        let packet_count = u16::try_from(chunk_count)
            .map_err(|_| ProtocolError::Malformed("frame needs more than u16::MAX packets"))?;

        let frame_id = self.next_frame_id;
        self.next_frame_id = self.next_frame_id.wrapping_add(1);

        // One allocation for the whole frame: every datagram is a slice of
        // it, and it is freed when the last datagram has been sent.
        let mut frame = BytesMut::with_capacity(data.len() + chunk_count * HEADER_LEN);
        // Not `data.chunks()`: an empty frame still needs one (empty) chunk.
        let datagrams = (0..packet_count)
            .map(|packet_index| {
                let start = (usize::from(packet_index) * payload_size).min(data.len());
                let chunk = &data[start..(start + payload_size).min(data.len())];
                PacketHeader {
                    keyframe,
                    frame_id,
                    packet_index,
                    packet_count,
                    timestamp,
                }
                .write(&mut frame);
                frame.put_slice(chunk);
                frame.split_to(HEADER_LEN + chunk.len()).freeze()
            })
            .collect();
        Ok(datagrams)
    }
}

impl Default for Packetizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Rate-limits keyframe requests so a burst of losses yields one request,
/// while a request that got lost/ignored is retried after `min_interval`.
pub struct KeyframeRequester {
    min_interval: Duration,
    last_request: Option<Instant>,
}

impl KeyframeRequester {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_request: None,
        }
    }

    /// Returns true if a request should be sent now (no request sent within
    /// `min_interval`); records it as sent.
    pub fn should_request(&mut self, now: Instant) -> bool {
        let allowed = self
            .last_request
            .is_none_or(|last| now.saturating_duration_since(last) >= self.min_interval);
        if allowed {
            self.last_request = Some(now);
        }
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn header(frame_id: u32, index: u16, count: u16, keyframe: bool) -> PacketHeader {
        PacketHeader {
            keyframe,
            frame_id,
            packet_index: index,
            packet_count: count,
            timestamp: 7,
        }
    }

    pub(super) fn datagram(h: PacketHeader, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        h.write(&mut out);
        out.extend_from_slice(payload);
        out
    }

    /// Single-packet frame datagram with payload `[frame_id as u8]`.
    pub(super) fn single(frame_id: u32, keyframe: bool) -> Vec<u8> {
        datagram(header(frame_id, 0, 1, keyframe), &[frame_id as u8])
    }

    // ---- header ----

    #[test]
    fn header_roundtrip() {
        let h = PacketHeader {
            keyframe: true,
            frame_id: 0xDEAD_BEEF,
            packet_index: 3,
            packet_count: 0xABCD,
            timestamp: -1234567890123,
        };
        let bytes = datagram(h, b"payload");
        assert_eq!(bytes.len(), HEADER_LEN + 7);
        assert_eq!(PacketHeader::parse(&bytes).unwrap(), (h, &b"payload"[..]));
    }

    #[test]
    fn header_layout_is_little_endian() {
        let mut bytes = Vec::new();
        header(0x0403_0201, 0x0605, 0x0807, false).write(&mut bytes);
        assert_eq!(&bytes[..10], &[VIDEO_VERSION, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&bytes[10..], &7i64.to_le_bytes());
    }

    #[test]
    fn header_truncated() {
        let bytes = single(1, false);
        assert_eq!(
            PacketHeader::parse(&bytes[..HEADER_LEN - 1]),
            Err(ProtocolError::Truncated(17))
        );
        assert_eq!(PacketHeader::parse(&[]), Err(ProtocolError::Truncated(0)));
        assert!(
            PacketHeader::parse(&bytes[..HEADER_LEN])
                .unwrap()
                .1
                .is_empty()
        );
    }

    #[test]
    fn header_bad_version() {
        let mut bytes = single(1, false);
        bytes[0] = 9;
        assert_eq!(
            PacketHeader::parse(&bytes),
            Err(ProtocolError::BadVersion(9))
        );
    }

    #[test]
    fn header_zero_count_is_malformed() {
        let bytes = datagram(header(1, 0, 0, false), &[]);
        assert!(matches!(
            PacketHeader::parse(&bytes),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn header_ignores_unknown_flags() {
        let mut bytes = single(1, false);
        bytes[1] = 0b1000_0001;
        assert!(PacketHeader::parse(&bytes).unwrap().0.keyframe);
    }

    // ---- packetizer ----

    fn parsed(datagrams: &[Bytes]) -> Vec<(PacketHeader, Vec<u8>)> {
        datagrams
            .iter()
            .map(|d| {
                let (h, p) = PacketHeader::parse(d).unwrap();
                (h, p.to_vec())
            })
            .collect()
    }

    #[test]
    fn packetize_exact_fit_is_one_packet() {
        let data = vec![5u8; 100];
        let out = Packetizer::new()
            .packetize(&data, true, 42, HEADER_LEN + 100)
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), HEADER_LEN + 100);
        let (h, p) = &parsed(&out)[0];
        assert_eq!(
            *h,
            PacketHeader {
                keyframe: true,
                frame_id: 0,
                packet_index: 0,
                packet_count: 1,
                timestamp: 42
            }
        );
        assert_eq!(p, &data);
    }

    #[test]
    fn packetize_one_byte_over_boundary() {
        let data: Vec<u8> = (0..101).collect();
        let out = Packetizer::new()
            .packetize(&data, false, 0, HEADER_LEN + 100)
            .unwrap();
        assert_eq!(
            out.iter().map(Bytes::len).collect::<Vec<_>>(),
            vec![HEADER_LEN + 100, HEADER_LEN + 1]
        );
        let packets = parsed(&out);
        assert!(
            packets
                .iter()
                .all(|(h, _)| h.packet_count == 2 && !h.keyframe)
        );
        assert_eq!(packets[1].0.packet_index, 1);
        let joined: Vec<u8> = packets.into_iter().flat_map(|(_, p)| p).collect();
        assert_eq!(joined, data);
    }

    #[test]
    fn packetize_exact_multiple() {
        let out = Packetizer::new()
            .packetize(&[1; 300], false, 0, HEADER_LEN + 100)
            .unwrap();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|d| d.len() == HEADER_LEN + 100));
    }

    #[test]
    fn packetize_empty_frame_yields_one_datagram() {
        let out = Packetizer::new()
            .packetize(&[], true, 0, HEADER_LEN + 1)
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), HEADER_LEN);
        assert_eq!(parsed(&out)[0].0.packet_count, 1);
    }

    #[test]
    fn packetize_assigns_consecutive_frame_ids() {
        let mut p = Packetizer::new();
        let ids: Vec<u32> = (0..3)
            .map(|_| {
                parsed(&p.packetize(&[1, 2, 3], false, 0, 100).unwrap())[0]
                    .0
                    .frame_id
            })
            .collect();
        assert_eq!(ids, vec![0, 1, 2]);
        assert_eq!(p.next_frame_id(), 3);
    }

    #[test]
    fn packetize_max_packet_count_boundary() {
        let mut p = Packetizer::new();
        let max = usize::from(u16::MAX);
        let out = p
            .packetize(&vec![0; max], false, 0, HEADER_LEN + 1)
            .unwrap();
        assert_eq!(out.len(), max);
        assert_eq!(parsed(&out[max - 1..])[0].0.packet_index, u16::MAX - 1);

        assert!(matches!(
            p.packetize(&vec![0; max + 1], false, 0, HEADER_LEN + 1),
            Err(ProtocolError::Malformed(_))
        ));
        // The failed frame did not consume an id.
        assert_eq!(
            parsed(&p.packetize(&[], false, 0, 100).unwrap())[0]
                .0
                .frame_id,
            1
        );
    }

    #[test]
    #[should_panic]
    fn packetize_panics_when_header_does_not_fit() {
        let _ = Packetizer::new().packetize(&[1], false, 0, HEADER_LEN);
    }

    // ---- keyframe requester ----

    #[test]
    fn keyframe_requester_rate_limits() {
        let base = Instant::now();
        let ms = |n| base + Duration::from_millis(n);
        let mut k = KeyframeRequester::new(Duration::from_millis(250));
        assert!(k.should_request(ms(0)));
        assert!(!k.should_request(ms(0)));
        assert!(!k.should_request(ms(249)));
        assert!(k.should_request(ms(250)));
        assert!(!k.should_request(ms(400)));
        assert!(k.should_request(ms(1000)));
    }

    #[test]
    fn keyframe_requester_tolerates_time_going_backwards() {
        let base = Instant::now() + Duration::from_secs(1);
        let mut k = KeyframeRequester::new(Duration::from_millis(250));
        assert!(k.should_request(base));
        assert!(!k.should_request(base - Duration::from_millis(500)));
    }
}
