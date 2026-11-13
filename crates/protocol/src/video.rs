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

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

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
    pub fn write(&self, out: &mut Vec<u8>) {
        let flags = if self.keyframe { FLAG_KEYFRAME } else { 0 };
        out.reserve(HEADER_LEN);
        out.push(VIDEO_VERSION);
        out.push(flags);
        out.extend_from_slice(&self.frame_id.to_le_bytes());
        out.extend_from_slice(&self.packet_index.to_le_bytes());
        out.extend_from_slice(&self.packet_count.to_le_bytes());
        out.extend_from_slice(&self.timestamp.to_le_bytes());
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
    pub data: Vec<u8>,
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
    ) -> Result<Vec<Vec<u8>>, ProtocolError> {
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

        let chunks: Vec<&[u8]> = if data.is_empty() {
            vec![&[]]
        } else {
            data.chunks(payload_size).collect()
        };
        let datagrams = chunks
            .into_iter()
            .zip(0u16..)
            .map(|(chunk, packet_index)| {
                let mut datagram = Vec::with_capacity(HEADER_LEN + chunk.len());
                PacketHeader {
                    keyframe,
                    frame_id,
                    packet_index,
                    packet_count,
                    timestamp,
                }
                .write(&mut datagram);
                datagram.extend_from_slice(chunk);
                datagram
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReassemblyEvent {
    /// A complete frame that is safe to decode.
    Frame(ReassembledFrame),
    /// The frame with this id will never be complete.
    Lost { frame_id: u32 },
    /// A complete frame that was discarded because a frame it depends on
    /// was lost and no keyframe has arrived since.
    Skipped { frame_id: u32 },
}

/// One frame whose packets are still arriving.
struct PendingFrame {
    keyframe: bool,
    timestamp: i64,
    packets: Vec<Option<Vec<u8>>>,
    received: usize,
}

impl PendingFrame {
    fn new(header: &PacketHeader) -> Self {
        Self {
            keyframe: header.keyframe,
            timestamp: header.timestamp,
            packets: vec![None; usize::from(header.packet_count)],
            received: 0,
        }
    }

    /// Stores a packet; returns false for duplicates or count mismatches.
    fn insert(&mut self, header: &PacketHeader, payload: &[u8]) -> bool {
        if usize::from(header.packet_count) != self.packets.len() {
            return false;
        }
        let slot = &mut self.packets[usize::from(header.packet_index)];
        if slot.is_some() {
            return false;
        }
        *slot = Some(payload.to_vec());
        self.received += 1;
        true
    }

    fn is_complete(&self) -> bool {
        self.received == self.packets.len()
    }

    fn into_frame(self, frame_id: u32) -> ReassembledFrame {
        let data = self.packets.into_iter().flatten().flatten().collect();
        ReassembledFrame {
            frame_id,
            timestamp: self.timestamp,
            keyframe: self.keyframe,
            data,
        }
    }
}

/// Rebuilds frames from datagrams, optimising for latency over completeness.
///
/// Policy:
/// - Frames are emitted in frame-id order; datagrams may arrive out of order.
/// - When a frame completes while older frames are still incomplete, those
///   older frames are declared `Lost` (in LAN, reordering across frames is
///   rare; waiting would only add latency).
/// - At most `max_pending` incomplete frames are kept; when exceeded, the
///   oldest is declared `Lost`.
/// - Datagrams for frames older than the last emitted/lost one, duplicates
///   and inconsistent packets (index >= count, count mismatch) are ignored.
/// - It starts "awaiting keyframe" and enters that state again after any
///   loss: until a keyframe completes, complete non-keyframes are reported
///   as `Skipped` instead of `Frame`.
///
/// Additionally, once a frame has been emitted/lost, frame ids that are
/// skipped entirely (every packet dropped) are also reported as `Lost` when a
/// later frame resolves, because ids are contiguous. At most
/// [`MAX_REPORTED_GAP`] such never-seen ids are reported per gap. `max_pending`
/// values below 1 are treated as 1.
pub struct Reassembler {
    max_pending: usize,
    pending: BTreeMap<u32, PendingFrame>,
    /// Highest frame id that was emitted, skipped or declared lost.
    last_resolved: Option<u32>,
    awaiting_keyframe: bool,
}

impl Reassembler {
    pub fn new(max_pending: usize) -> Self {
        Self {
            max_pending: max_pending.max(1),
            pending: BTreeMap::new(),
            last_resolved: None,
            awaiting_keyframe: true,
        }
    }

    /// Processes one datagram. Malformed datagrams return an error and leave
    /// the state unchanged.
    pub fn push(&mut self, datagram: &[u8]) -> Result<Vec<ReassemblyEvent>, ProtocolError> {
        let (header, payload) = PacketHeader::parse(datagram)?;
        let mut events = Vec::new();
        if self.is_resolved(header.frame_id) || header.packet_index >= header.packet_count {
            return Ok(events);
        }

        let frame_id = header.frame_id;
        let frame = self
            .pending
            .entry(frame_id)
            .or_insert_with(|| PendingFrame::new(&header));
        if !frame.insert(&header, payload) {
            return Ok(events);
        }

        if frame.is_complete() {
            let frame = self.pending.remove(&frame_id).expect("frame is pending");
            self.resolve_older_than(frame_id, &mut events);
            events.push(self.classify(frame.into_frame(frame_id)));
            self.last_resolved = Some(frame_id);
        } else if self.pending.len() > self.max_pending {
            let (&oldest, _) = self
                .pending
                .first_key_value()
                .expect("pending is not empty");
            self.resolve_older_than(oldest, &mut events);
            self.pending.remove(&oldest);
            self.declare_lost(oldest, &mut events);
            self.last_resolved = Some(oldest);
        }
        Ok(events)
    }

    /// True until a keyframe completes, and again after any loss.
    pub fn awaiting_keyframe(&self) -> bool {
        self.awaiting_keyframe
    }

    fn is_resolved(&self, frame_id: u32) -> bool {
        self.last_resolved.is_some_and(|last| frame_id <= last)
    }

    /// Declares every unresolved frame id below `frame_id` lost: incomplete
    /// pending frames and (bounded) ids that were never seen at all.
    fn resolve_older_than(&mut self, frame_id: u32, events: &mut Vec<ReassemblyEvent>) {
        let still_pending = self.pending.split_off(&frame_id);
        let mut lost: BTreeSet<u32> = std::mem::replace(&mut self.pending, still_pending)
            .into_keys()
            .collect();
        if let Some(last) = self.last_resolved {
            let first_gap = last + 1;
            let gap_end = frame_id.min(first_gap.saturating_add(MAX_REPORTED_GAP));
            lost.extend(first_gap..gap_end);
        }
        for id in lost {
            self.declare_lost(id, events);
        }
    }

    fn declare_lost(&mut self, frame_id: u32, events: &mut Vec<ReassemblyEvent>) {
        self.awaiting_keyframe = true;
        events.push(ReassemblyEvent::Lost { frame_id });
    }

    /// Applies the keyframe dependency rule to a complete frame.
    fn classify(&mut self, frame: ReassembledFrame) -> ReassemblyEvent {
        if frame.keyframe {
            self.awaiting_keyframe = false;
        }
        if self.awaiting_keyframe {
            ReassemblyEvent::Skipped {
                frame_id: frame.frame_id,
            }
        } else {
            ReassemblyEvent::Frame(frame)
        }
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

    fn header(frame_id: u32, index: u16, count: u16, keyframe: bool) -> PacketHeader {
        PacketHeader {
            keyframe,
            frame_id,
            packet_index: index,
            packet_count: count,
            timestamp: 7,
        }
    }

    fn datagram(h: PacketHeader, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        h.write(&mut out);
        out.extend_from_slice(payload);
        out
    }

    /// Single-packet frame datagram with payload `[frame_id as u8]`.
    fn single(frame_id: u32, keyframe: bool) -> Vec<u8> {
        datagram(header(frame_id, 0, 1, keyframe), &[frame_id as u8])
    }

    fn frame_event(frame_id: u32, keyframe: bool, data: &[u8]) -> ReassemblyEvent {
        ReassemblyEvent::Frame(ReassembledFrame {
            frame_id,
            timestamp: 7,
            keyframe,
            data: data.to_vec(),
        })
    }

    fn lost(frame_id: u32) -> ReassemblyEvent {
        ReassemblyEvent::Lost { frame_id }
    }

    /// Reassembler that already emitted keyframe 0, so it is not awaiting one.
    fn primed(max_pending: usize) -> Reassembler {
        let mut r = Reassembler::new(max_pending);
        assert_eq!(
            r.push(&single(0, true)).unwrap(),
            vec![frame_event(0, true, &[0])]
        );
        r
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

    fn parsed(datagrams: &[Vec<u8>]) -> Vec<(PacketHeader, Vec<u8>)> {
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
            out.iter().map(Vec::len).collect::<Vec<_>>(),
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

    // ---- reassembler ----

    #[test]
    fn reassembles_in_order() {
        let mut r = Reassembler::new(8);
        let data: Vec<u8> = (0..250).collect();
        let packets = Packetizer::new()
            .packetize(&data, true, 7, HEADER_LEN + 100)
            .unwrap();
        assert!(r.push(&packets[0]).unwrap().is_empty());
        assert!(r.push(&packets[1]).unwrap().is_empty());
        assert_eq!(
            r.push(&packets[2]).unwrap(),
            vec![frame_event(0, true, &data)]
        );
        assert!(!r.awaiting_keyframe());
    }

    #[test]
    fn reassembles_out_of_order_within_frame() {
        let mut r = Reassembler::new(8);
        let data: Vec<u8> = (0..250).collect();
        let packets = Packetizer::new()
            .packetize(&data, true, 7, HEADER_LEN + 100)
            .unwrap();
        assert!(r.push(&packets[2]).unwrap().is_empty());
        assert!(r.push(&packets[0]).unwrap().is_empty());
        assert_eq!(
            r.push(&packets[1]).unwrap(),
            vec![frame_event(0, true, &data)]
        );
    }

    #[test]
    fn duplicates_are_ignored() {
        let mut r = Reassembler::new(8);
        let a = datagram(header(0, 0, 2, true), &[1]);
        let a_dup = datagram(header(0, 0, 2, true), &[9]);
        assert!(r.push(&a).unwrap().is_empty());
        assert!(r.push(&a_dup).unwrap().is_empty());
        let b = datagram(header(0, 1, 2, true), &[2]);
        assert_eq!(r.push(&b).unwrap(), vec![frame_event(0, true, &[1, 2])]);
        // Duplicate of an already emitted frame.
        assert!(r.push(&b).unwrap().is_empty());
    }

    #[test]
    fn starts_awaiting_keyframe_and_skips_until_one() {
        let mut r = Reassembler::new(8);
        assert!(r.awaiting_keyframe());
        assert_eq!(
            r.push(&single(5, false)).unwrap(),
            vec![ReassemblyEvent::Skipped { frame_id: 5 }]
        );
        assert!(r.awaiting_keyframe());
        assert_eq!(
            r.push(&single(6, true)).unwrap(),
            vec![frame_event(6, true, &[6])]
        );
        assert_eq!(
            r.push(&single(7, false)).unwrap(),
            vec![frame_event(7, false, &[7])]
        );
        assert!(!r.awaiting_keyframe());
    }

    #[test]
    fn older_incomplete_frame_is_lost_when_newer_completes() {
        let mut r = primed(8);
        assert!(
            r.push(&datagram(header(1, 0, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.push(&single(2, false)).unwrap(),
            vec![lost(1), ReassemblyEvent::Skipped { frame_id: 2 }]
        );
        assert!(r.awaiting_keyframe());
        // Late packet of the lost frame is ignored.
        assert!(
            r.push(&datagram(header(1, 1, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.push(&single(3, true)).unwrap(),
            vec![frame_event(3, true, &[3])]
        );
        assert!(!r.awaiting_keyframe());
    }

    #[test]
    fn entirely_missing_frames_are_lost() {
        let mut r = primed(8);
        assert_eq!(
            r.push(&single(3, false)).unwrap(),
            vec![lost(1), lost(2), ReassemblyEvent::Skipped { frame_id: 3 }]
        );
    }

    #[test]
    fn huge_gap_reports_bounded_losses() {
        let mut r = primed(8);
        let events = r.push(&single(u32::MAX, false)).unwrap();
        assert_eq!(events.len(), MAX_REPORTED_GAP as usize + 1);
        assert_eq!(events[0], lost(1));
        assert_eq!(
            *events.last().unwrap(),
            ReassemblyEvent::Skipped { frame_id: u32::MAX }
        );
    }

    #[test]
    fn first_frames_without_history_report_only_pending_losses() {
        let mut r = Reassembler::new(8);
        assert!(
            r.push(&datagram(header(10, 0, 2, true), &[1]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.push(&single(12, false)).unwrap(),
            vec![lost(10), ReassemblyEvent::Skipped { frame_id: 12 }]
        );
    }

    #[test]
    fn max_pending_evicts_oldest() {
        let mut r = primed(2);
        assert!(
            r.push(&datagram(header(1, 0, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        assert!(
            r.push(&datagram(header(2, 0, 2, false), &[2]))
                .unwrap()
                .is_empty()
        );
        assert!(!r.awaiting_keyframe());
        assert_eq!(
            r.push(&datagram(header(3, 0, 2, false), &[3])).unwrap(),
            vec![lost(1)]
        );
        assert!(r.awaiting_keyframe());
        // Evicted frame's remaining packet is ignored.
        assert!(
            r.push(&datagram(header(1, 1, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        // Frame 2 can still complete (but must be skipped).
        assert_eq!(
            r.push(&datagram(header(2, 1, 2, false), &[2])).unwrap(),
            vec![ReassemblyEvent::Skipped { frame_id: 2 }]
        );
    }

    #[test]
    fn max_pending_zero_behaves_as_one() {
        let mut r = primed(0);
        assert!(
            r.push(&datagram(header(1, 0, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.push(&datagram(header(2, 0, 2, false), &[2])).unwrap(),
            vec![lost(1)]
        );
    }

    #[test]
    fn inconsistent_packets_are_ignored() {
        let mut r = primed(8);
        // index >= count
        assert!(
            r.push(&datagram(header(1, 2, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        assert!(
            r.push(&datagram(header(1, 0, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        // count mismatch with the first packet of the frame
        assert!(
            r.push(&datagram(header(1, 1, 3, false), &[1]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.push(&datagram(header(1, 1, 2, false), &[2])).unwrap(),
            vec![frame_event(1, false, &[1, 2])]
        );
    }

    #[test]
    fn malformed_datagram_errors_and_keeps_state() {
        let mut r = primed(8);
        assert!(
            r.push(&datagram(header(1, 0, 2, false), &[1]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            r.push(&[VIDEO_VERSION, 0]),
            Err(ProtocolError::Truncated(2))
        );
        let mut bad = datagram(header(1, 1, 2, false), &[2]);
        bad[0] = 0;
        assert_eq!(r.push(&bad), Err(ProtocolError::BadVersion(0)));
        assert!(!r.awaiting_keyframe());
        assert_eq!(
            r.push(&datagram(header(1, 1, 2, false), &[2])).unwrap(),
            vec![frame_event(1, false, &[1, 2])]
        );
    }

    #[test]
    fn packetizer_reassembler_roundtrip_many_frames() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(8);
        for i in 0..20u32 {
            let data: Vec<u8> = (0..(i * 37)).map(|b| b as u8).collect();
            let mut packets = p
                .packetize(&data, i == 0, i64::from(i), HEADER_LEN + 50)
                .unwrap();
            packets.reverse();
            let events: Vec<_> = packets.iter().flat_map(|d| r.push(d).unwrap()).collect();
            assert_eq!(
                events,
                vec![ReassemblyEvent::Frame(ReassembledFrame {
                    frame_id: i,
                    timestamp: i64::from(i),
                    keyframe: i == 0,
                    data
                })]
            );
        }
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
