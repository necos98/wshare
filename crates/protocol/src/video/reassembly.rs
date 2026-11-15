//! Rebuilding frames from datagrams, and the loss policy.

use std::collections::BTreeMap;

use bytes::{BufMut, Bytes, BytesMut};

use super::{HEADER_LEN, MAX_REPORTED_GAP, PacketHeader, ReassembledFrame};
use crate::ProtocolError;
use crate::recovery::{MAX_NACK_PACKETS, Missing};

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

/// A frame still waiting for some of its datagrams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    pub frame_id: u32,
    pub missing: Missing,
    /// The newest frame seen so far: its last datagrams may simply not have
    /// arrived yet. For any older frame, a newer datagram already arrived,
    /// so what is missing was lost (or reordered, which a LAN rarely does).
    pub newest: bool,
}

/// One frame whose packets are still arriving.
struct PendingFrame {
    keyframe: bool,
    timestamp: i64,
    /// Payloads, as slices of the received datagrams (no copy).
    packets: Vec<Option<Bytes>>,
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
    fn insert(&mut self, header: &PacketHeader, payload: Bytes) -> bool {
        if usize::from(header.packet_count) != self.packets.len() {
            return false;
        }
        let slot = &mut self.packets[usize::from(header.packet_index)];
        if slot.is_some() {
            return false;
        }
        *slot = Some(payload);
        self.received += 1;
        true
    }

    fn is_complete(&self) -> bool {
        self.received == self.packets.len()
    }

    /// The packets not received yet, as a [`Missing`] (which lists at most
    /// [`MAX_NACK_PACKETS`]: only one more is collected to tell).
    fn missing(&self) -> Missing {
        let indices = self
            .packets
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_none())
            .map(|(index, _)| u16::try_from(index).expect("packet count is a u16"))
            .take(MAX_NACK_PACKETS + 1)
            .collect();
        Missing::from_indices(indices)
    }

    /// Joins the payloads: a single-packet frame is handed over as is, a
    /// larger one is copied once into a buffer of the exact size.
    fn into_frame(self, frame_id: u32) -> ReassembledFrame {
        let mut payloads: Vec<Bytes> = self.packets.into_iter().flatten().collect();
        let data = if payloads.len() == 1 {
            payloads.swap_remove(0)
        } else {
            let mut joined = BytesMut::with_capacity(payloads.iter().map(Bytes::len).sum());
            for payload in &payloads {
                joined.put_slice(payload);
            }
            joined.freeze()
        };
        ReassembledFrame {
            frame_id,
            timestamp: self.timestamp,
            keyframe: self.keyframe,
            data,
        }
    }
}

/// A frame id that is not resolved yet.
enum Entry {
    /// Some datagrams are still missing.
    Partial(PendingFrame),
    /// Complete, but held until every older frame is resolved: frames are
    /// decoded in order.
    Complete(ReassembledFrame),
}

/// Rebuilds frames from datagrams and decides when a frame is lost.
///
/// Policy:
/// - Frames are emitted in frame-id order; datagrams may arrive out of
///   order. A complete frame is held while an older one is unresolved, so
///   the decoder always gets the frames it depends on first.
/// - An incomplete frame is never given up by the arrival of newer ones:
///   its missing datagrams can be sent again ([`Self::gaps`] tells which).
///   The caller decides when to stop waiting and calls [`Self::give_up`].
/// - A complete keyframe needs no older frame: every older unresolved frame
///   is declared `Lost` at once and the keyframe is emitted.
/// - At most `max_pending` consecutive frame ids are unresolved: a datagram
///   further ahead declares the oldest ones lost to make room.
/// - Datagrams for frames older than the last emitted/lost one, duplicates
///   and inconsistent packets (index >= count, count mismatch) are ignored.
/// - It starts "awaiting keyframe" and enters that state again after any
///   loss: until a keyframe completes, complete non-keyframes are reported
///   as `Skipped` instead of `Frame`.
///
/// Once a frame has been resolved, frame ids that never showed up at all
/// (every packet dropped) are unresolved like incomplete frames, because ids
/// are contiguous; when they are given up, at most [`MAX_REPORTED_GAP`] of
/// them are reported as `Lost` per gap. `max_pending` values below 1 are
/// treated as 1.
pub struct Reassembler {
    max_pending: usize,
    entries: BTreeMap<u32, Entry>,
    /// Highest frame id that was emitted, skipped or declared lost.
    last_resolved: Option<u32>,
    /// Highest frame id a datagram was accepted for.
    highest_seen: Option<u32>,
    awaiting_keyframe: bool,
}

impl Reassembler {
    pub fn new(max_pending: usize) -> Self {
        Self {
            max_pending: max_pending.max(1),
            entries: BTreeMap::new(),
            last_resolved: None,
            highest_seen: None,
            awaiting_keyframe: true,
        }
    }

    /// Processes one datagram. Malformed datagrams return an error and leave
    /// the state unchanged.
    pub fn push(
        &mut self,
        datagram: impl Into<Bytes>,
    ) -> Result<Vec<ReassemblyEvent>, ProtocolError> {
        let datagram = datagram.into();
        let (header, _) = PacketHeader::parse(&datagram)?;
        let payload = datagram.slice(HEADER_LEN..);
        let mut events = Vec::new();
        let frame_id = header.frame_id;
        if self.is_too_old(frame_id) || header.packet_index >= header.packet_count {
            return Ok(events);
        }

        self.make_room_for(frame_id, &mut events);
        self.highest_seen = Some(self.highest_seen.map_or(frame_id, |h| h.max(frame_id)));
        let entry = self
            .entries
            .entry(frame_id)
            .or_insert_with(|| Entry::Partial(PendingFrame::new(&header)));
        let Entry::Partial(frame) = entry else {
            return Ok(events); // duplicate of a held complete frame
        };
        if frame.insert(&header, payload) && frame.is_complete() {
            self.complete(frame_id, &mut events);
        }
        self.release(&mut events);
        Ok(events)
    }

    /// Stops waiting for every unresolved frame up to `frame_id`: incomplete
    /// and never-seen ones are declared `Lost`, then the complete frames
    /// held behind them are released (as `Skipped`, unless one is a
    /// keyframe).
    pub fn give_up(&mut self, frame_id: u32) -> Vec<ReassemblyEvent> {
        let mut events = Vec::new();
        self.resolve_through(frame_id, &mut events);
        self.release(&mut events);
        events
    }

    /// The unresolved frames still missing datagrams, oldest first: at most
    /// `max_pending`. A frame none of whose datagrams arrived is
    /// [`Missing::Whole`].
    pub fn gaps(&self) -> Vec<Gap> {
        let (Some(first), Some(highest)) = (self.first_unresolved(), self.highest_seen) else {
            return Vec::new();
        };
        (first..=highest)
            .take(self.max_pending)
            .filter_map(|frame_id| {
                let missing = match self.entries.get(&frame_id) {
                    Some(Entry::Complete(_)) => return None,
                    Some(Entry::Partial(frame)) => frame.missing(),
                    None => Missing::Whole,
                };
                Some(Gap {
                    frame_id,
                    missing,
                    newest: frame_id == highest,
                })
            })
            .collect()
    }

    /// True until a keyframe completes, and again after any loss.
    pub fn awaiting_keyframe(&self) -> bool {
        self.awaiting_keyframe
    }

    /// Whether `frame_id` was emitted, skipped or declared lost: nothing
    /// more will be done with it.
    pub fn is_resolved(&self, frame_id: u32) -> bool {
        self.last_resolved.is_some_and(|last| frame_id <= last)
    }

    /// Resolved, or so far behind the newest frame that waiting for it
    /// would break the `max_pending` bound (possible only before anything
    /// was resolved, when older ids are still accepted).
    fn is_too_old(&self, frame_id: u32) -> bool {
        self.is_resolved(frame_id)
            || self
                .highest_seen
                .is_some_and(|highest| highest.saturating_sub(frame_id) >= self.window())
    }

    fn window(&self) -> u32 {
        u32::try_from(self.max_pending).unwrap_or(u32::MAX)
    }

    /// The oldest frame id not resolved yet. Before anything was resolved,
    /// the oldest frame seen: ids before it are unknown.
    fn first_unresolved(&self) -> Option<u32> {
        match self.last_resolved {
            Some(last) => last.checked_add(1),
            None => self.entries.keys().next().copied(),
        }
    }

    /// Keeps `frame_id` within `max_pending` ids of the oldest unresolved
    /// one, declaring older ids lost if needed.
    fn make_room_for(&mut self, frame_id: u32, events: &mut Vec<ReassemblyEvent>) {
        let Some(first) = self.first_unresolved() else {
            return;
        };
        let window = self.window();
        if frame_id >= first && frame_id - first >= window {
            self.resolve_through(frame_id - window, events);
        }
    }

    /// The pending frame `frame_id` just completed: it is held as complete
    /// (a keyframe first makes every older frame lost, it needs none).
    fn complete(&mut self, frame_id: u32, events: &mut Vec<ReassemblyEvent>) {
        let Some(Entry::Partial(frame)) = self.entries.remove(&frame_id) else {
            unreachable!("only a pending frame completes");
        };
        let frame = frame.into_frame(frame_id);
        if frame.keyframe
            && let Some(previous) = frame_id.checked_sub(1)
        {
            self.resolve_through(previous, events);
        }
        self.entries.insert(frame_id, Entry::Complete(frame));
    }

    /// Resolves every unresolved id up to `last`, in order: held complete
    /// frames are classified, incomplete and never-seen ones are lost.
    fn resolve_through(&mut self, last: u32, events: &mut Vec<ReassemblyEvent>) {
        if self.is_resolved(last) {
            return;
        }
        let mut next = self.first_unresolved();
        let newer = match last.checked_add(1) {
            Some(after) => self.entries.split_off(&after),
            None => BTreeMap::new(),
        };
        let older = std::mem::replace(&mut self.entries, newer);
        for (frame_id, entry) in older {
            if let Some(from) = next {
                self.lose_never_seen(from, frame_id, events);
            }
            match entry {
                Entry::Partial(_) => self.declare_lost(frame_id, events),
                Entry::Complete(frame) => events.push(self.classify(frame)),
            }
            next = frame_id.checked_add(1);
        }
        if let (Some(from), Some(end)) = (next, last.checked_add(1)) {
            self.lose_never_seen(from, end, events);
        }
        self.last_resolved = Some(last);
    }

    /// Declares the never-seen ids `from..end` lost, at most
    /// [`MAX_REPORTED_GAP`] of them individually.
    fn lose_never_seen(&mut self, from: u32, end: u32, events: &mut Vec<ReassemblyEvent>) {
        if from >= end {
            return;
        }
        let reported_end = end.min(from.saturating_add(MAX_REPORTED_GAP));
        for frame_id in from..reported_end {
            self.declare_lost(frame_id, events);
        }
        // Ids beyond the report bound are lost all the same.
        self.awaiting_keyframe = true;
    }

    /// Emits the complete frames that are next in order.
    fn release(&mut self, events: &mut Vec<ReassemblyEvent>) {
        while let Some(entry) = self.entries.first_entry() {
            let frame_id = *entry.key();
            let is_next = self
                .last_resolved
                .is_none_or(|last| last.checked_add(1) == Some(frame_id));
            if !is_next || !matches!(entry.get(), Entry::Complete(_)) {
                return;
            }
            let Entry::Complete(frame) = entry.remove() else {
                unreachable!("checked above");
            };
            events.push(self.classify(frame));
            self.last_resolved = Some(frame_id);
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

#[cfg(test)]
mod tests;
