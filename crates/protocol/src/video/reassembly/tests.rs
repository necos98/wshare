//! Tests of [`super::Reassembler`].

use super::super::Packetizer;
use super::super::tests::{datagram, header, single};
use super::*;

fn frame_event(frame_id: u32, keyframe: bool, data: &[u8]) -> ReassemblyEvent {
    ReassemblyEvent::Frame(ReassembledFrame {
        frame_id,
        timestamp: 7,
        keyframe,
        data: Bytes::copy_from_slice(data),
    })
}

fn lost(frame_id: u32) -> ReassemblyEvent {
    ReassemblyEvent::Lost { frame_id }
}

fn skipped(frame_id: u32) -> ReassemblyEvent {
    ReassemblyEvent::Skipped { frame_id }
}

fn gap(frame_id: u32, missing: Missing, newest: bool) -> Gap {
    Gap {
        frame_id,
        missing,
        newest,
    }
}

/// Reassembler that already emitted keyframe 0, so it is not awaiting one.
fn primed(max_pending: usize) -> Reassembler {
    let mut r = Reassembler::new(max_pending);
    assert_eq!(
        r.push(single(0, true)).unwrap(),
        vec![frame_event(0, true, &[0])]
    );
    r
}

#[test]
fn reassembles_in_order() {
    let mut r = Reassembler::new(8);
    let data: Vec<u8> = (0..250).collect();
    let packets = Packetizer::new()
        .packetize(&data, true, 7, HEADER_LEN + 100)
        .unwrap();
    assert!(r.push(packets[0].clone()).unwrap().is_empty());
    assert!(r.push(packets[1].clone()).unwrap().is_empty());
    assert_eq!(
        r.push(packets[2].clone()).unwrap(),
        vec![frame_event(0, true, &data)]
    );
    assert!(!r.awaiting_keyframe());
    assert!(r.gaps().is_empty());
}

#[test]
fn reassembles_out_of_order_within_frame() {
    let mut r = Reassembler::new(8);
    let data: Vec<u8> = (0..250).collect();
    let packets = Packetizer::new()
        .packetize(&data, true, 7, HEADER_LEN + 100)
        .unwrap();
    assert!(r.push(packets[2].clone()).unwrap().is_empty());
    assert!(r.push(packets[0].clone()).unwrap().is_empty());
    assert_eq!(
        r.push(packets[1].clone()).unwrap(),
        vec![frame_event(0, true, &data)]
    );
}

#[test]
fn duplicates_are_ignored() {
    let mut r = Reassembler::new(8);
    let a = datagram(header(0, 0, 2, true), &[1]);
    let a_dup = datagram(header(0, 0, 2, true), &[9]);
    assert!(r.push(a.clone()).unwrap().is_empty());
    assert!(r.push(a_dup.clone()).unwrap().is_empty());
    let b = datagram(header(0, 1, 2, true), &[2]);
    assert_eq!(
        r.push(b.clone()).unwrap(),
        vec![frame_event(0, true, &[1, 2])]
    );
    // Duplicate of an already emitted frame.
    assert!(r.push(b.clone()).unwrap().is_empty());
}

#[test]
fn starts_awaiting_keyframe_and_skips_until_one() {
    let mut r = Reassembler::new(8);
    assert!(r.awaiting_keyframe());
    assert_eq!(r.push(single(5, false)).unwrap(), vec![skipped(5)]);
    assert!(r.awaiting_keyframe());
    assert_eq!(
        r.push(single(6, true)).unwrap(),
        vec![frame_event(6, true, &[6])]
    );
    assert_eq!(
        r.push(single(7, false)).unwrap(),
        vec![frame_event(7, false, &[7])]
    );
    assert!(!r.awaiting_keyframe());
}

#[test]
fn newer_complete_frames_wait_for_an_older_incomplete_one() {
    let mut r = primed(8);
    assert!(
        r.push(datagram(header(1, 0, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    assert!(r.push(single(2, false)).unwrap().is_empty());
    assert!(r.push(single(3, false)).unwrap().is_empty());
    assert!(!r.awaiting_keyframe(), "nothing is lost yet");
    assert_eq!(r.gaps(), vec![gap(1, Missing::Packets(vec![1]), false)]);
    // The datagram sent again completes frame 1: all three follow, in
    // order.
    assert_eq!(
        r.push(datagram(header(1, 1, 2, false), &[9])).unwrap(),
        vec![
            frame_event(1, false, &[1, 9]),
            frame_event(2, false, &[2]),
            frame_event(3, false, &[3]),
        ]
    );
    assert!(r.gaps().is_empty());
    assert!(!r.awaiting_keyframe());
}

#[test]
fn giving_up_loses_the_frame_and_skips_the_ones_held_behind_it() {
    let mut r = primed(8);
    r.push(datagram(header(1, 0, 2, false), &[1])).unwrap();
    r.push(single(2, false)).unwrap();
    assert_eq!(r.give_up(1), vec![lost(1), skipped(2)]);
    assert!(r.awaiting_keyframe());
    assert!(r.is_resolved(2));
    // A late packet of the lost frame is ignored.
    assert!(
        r.push(datagram(header(1, 1, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        r.push(single(3, true)).unwrap(),
        vec![frame_event(3, true, &[3])]
    );
    assert!(!r.awaiting_keyframe());
}

#[test]
fn giving_up_a_resolved_frame_changes_nothing() {
    let mut r = primed(8);
    assert!(r.give_up(0).is_empty());
    assert!(!r.awaiting_keyframe());
}

#[test]
fn a_complete_keyframe_does_not_wait_for_older_frames() {
    let mut r = primed(8);
    r.push(datagram(header(1, 0, 2, false), &[1])).unwrap();
    r.push(single(2, false)).unwrap();
    assert_eq!(
        r.push(single(4, true)).unwrap(),
        vec![lost(1), skipped(2), lost(3), frame_event(4, true, &[4])]
    );
    assert!(!r.awaiting_keyframe());
    assert!(r.gaps().is_empty());
}

#[test]
fn frames_never_seen_are_gaps_asked_for_whole() {
    let mut r = primed(8);
    assert!(r.push(single(3, false)).unwrap().is_empty());
    assert_eq!(
        r.gaps(),
        vec![gap(1, Missing::Whole, false), gap(2, Missing::Whole, false)]
    );
    assert_eq!(r.give_up(2), vec![lost(1), lost(2), skipped(3)]);
}

#[test]
fn the_newest_frame_is_flagged_as_possibly_still_arriving() {
    let mut r = primed(8);
    r.push(datagram(header(1, 0, 3, false), &[1])).unwrap();
    assert_eq!(r.gaps(), vec![gap(1, Missing::Packets(vec![1, 2]), true)]);
    r.push(datagram(header(2, 0, 2, false), &[2])).unwrap();
    assert_eq!(
        r.gaps(),
        vec![
            gap(1, Missing::Packets(vec![1, 2]), false),
            gap(2, Missing::Packets(vec![1]), true),
        ]
    );
}

#[test]
fn a_frame_missing_too_many_packets_is_asked_for_whole() {
    let mut r = primed(8);
    let count = MAX_NACK_PACKETS as u16 + 2;
    r.push(datagram(header(1, 0, count, false), &[1])).unwrap();
    assert_eq!(r.gaps(), vec![gap(1, Missing::Whole, true)]);
}

#[test]
fn huge_gap_reports_bounded_losses() {
    let mut r = primed(8);
    let events = r.push(single(u32::MAX, false)).unwrap();
    // Everything but the last `max_pending` ids is given up at once.
    assert_eq!(events.len(), MAX_REPORTED_GAP as usize);
    assert_eq!(events[0], lost(1));
    assert!(r.awaiting_keyframe());
    let gaps = r.gaps();
    assert_eq!(gaps.len(), 7);
    assert!(gaps.iter().all(|g| g.missing == Missing::Whole));
    assert_eq!(r.give_up(u32::MAX - 1).last(), Some(&skipped(u32::MAX)));
}

#[test]
fn first_frames_without_history_wait_for_the_frames_between() {
    let mut r = Reassembler::new(8);
    assert!(
        r.push(datagram(header(10, 0, 2, true), &[1]))
            .unwrap()
            .is_empty()
    );
    assert!(r.push(single(12, false)).unwrap().is_empty());
    assert_eq!(
        r.gaps(),
        vec![
            gap(10, Missing::Packets(vec![1]), false),
            gap(11, Missing::Whole, false)
        ]
    );
    assert_eq!(r.give_up(11), vec![lost(10), lost(11), skipped(12)]);
}

#[test]
fn max_pending_evicts_oldest() {
    let mut r = primed(2);
    assert!(
        r.push(datagram(header(1, 0, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    assert!(
        r.push(datagram(header(2, 0, 2, false), &[2]))
            .unwrap()
            .is_empty()
    );
    assert!(!r.awaiting_keyframe());
    assert_eq!(
        r.push(datagram(header(3, 0, 2, false), &[3])).unwrap(),
        vec![lost(1)]
    );
    assert!(r.awaiting_keyframe());
    // Evicted frame's remaining packet is ignored.
    assert!(
        r.push(datagram(header(1, 1, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    // Frame 2 can still complete (but must be skipped).
    assert_eq!(
        r.push(datagram(header(2, 1, 2, false), &[2])).unwrap(),
        vec![skipped(2)]
    );
}

#[test]
fn max_pending_zero_behaves_as_one() {
    let mut r = primed(0);
    assert!(
        r.push(datagram(header(1, 0, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        r.push(datagram(header(2, 0, 2, false), &[2])).unwrap(),
        vec![lost(1)]
    );
}

#[test]
fn ids_far_behind_the_newest_are_ignored_before_anything_resolved() {
    let mut r = Reassembler::new(4);
    r.push(datagram(header(100, 0, 2, true), &[1])).unwrap();
    // Within the window: accepted, it may be the frame before.
    r.push(datagram(header(97, 0, 2, false), &[1])).unwrap();
    assert_eq!(r.gaps().first().map(|g| g.frame_id), Some(97));
    for frame_id in (0..96).rev() {
        assert!(r.push(single(frame_id, false)).unwrap().is_empty());
    }
    assert_eq!(r.gaps().first().map(|g| g.frame_id), Some(97));
}

#[test]
fn a_duplicate_of_a_held_frame_is_ignored() {
    let mut r = primed(8);
    r.push(datagram(header(1, 0, 2, false), &[1])).unwrap();
    r.push(single(2, false)).unwrap();
    assert!(r.push(single(2, false)).unwrap().is_empty());
    assert_eq!(
        r.push(datagram(header(1, 1, 2, false), &[2])).unwrap(),
        vec![frame_event(1, false, &[1, 2]), frame_event(2, false, &[2])]
    );
}

#[test]
fn inconsistent_packets_are_ignored() {
    let mut r = primed(8);
    // index >= count
    assert!(
        r.push(datagram(header(1, 2, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    assert!(
        r.push(datagram(header(1, 0, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    // count mismatch with the first packet of the frame
    assert!(
        r.push(datagram(header(1, 1, 3, false), &[1]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        r.push(datagram(header(1, 1, 2, false), &[2])).unwrap(),
        vec![frame_event(1, false, &[1, 2])]
    );
}

#[test]
fn malformed_datagram_errors_and_keeps_state() {
    let mut r = primed(8);
    assert!(
        r.push(datagram(header(1, 0, 2, false), &[1]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        r.push(&[super::super::VIDEO_VERSION, 0][..]),
        Err(ProtocolError::Truncated(2))
    );
    let mut bad = datagram(header(1, 1, 2, false), &[2]);
    bad[0] = 0;
    assert_eq!(r.push(bad.clone()), Err(ProtocolError::BadVersion(0)));
    assert!(!r.awaiting_keyframe());
    assert_eq!(
        r.push(datagram(header(1, 1, 2, false), &[2])).unwrap(),
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
        let events: Vec<_> = packets
            .iter()
            .flat_map(|d| r.push(d.clone()).unwrap())
            .collect();
        assert_eq!(
            events,
            vec![ReassemblyEvent::Frame(ReassembledFrame {
                frame_id: i,
                timestamp: i64::from(i),
                keyframe: i == 0,
                data: data.into()
            })]
        );
    }
}

#[test]
fn packetizer_slices_one_allocation_per_frame() {
    let data: Vec<u8> = (0..250).map(|b| b as u8).collect();
    let out = Packetizer::new()
        .packetize(&data, true, 0, HEADER_LEN + 100)
        .unwrap();
    assert_eq!(out.len(), 3);
    for pair in out.windows(2) {
        let end_of_first = pair[0].as_ptr().wrapping_add(pair[0].len());
        assert_eq!(end_of_first, pair[1].as_ptr(), "datagrams are contiguous");
    }
}

#[test]
fn single_packet_frame_reuses_the_datagram_memory() {
    let datagram = Bytes::from(datagram(header(0, 0, 1, true), b"picture"));
    let payload_address = datagram[HEADER_LEN..].as_ptr();
    let events = Reassembler::new(4).push(datagram).unwrap();
    let [ReassemblyEvent::Frame(frame)] = &events[..] else {
        panic!("expected one frame, got {events:?}");
    };
    assert_eq!(&frame.data[..], b"picture");
    assert_eq!(frame.data.as_ptr(), payload_address, "no copy");
}

#[test]
fn multi_packet_frame_is_joined_into_an_exact_buffer() {
    let data: Vec<u8> = (0..1000u32).map(|b| (b * 7) as u8).collect();
    let mut packets = Packetizer::new()
        .packetize(&data, true, 3, HEADER_LEN + 64)
        .unwrap();
    packets.reverse();
    let mut r = Reassembler::new(4);
    let events: Vec<_> = packets
        .into_iter()
        .flat_map(|d| r.push(d).unwrap())
        .collect();
    let [ReassemblyEvent::Frame(frame)] = &events[..] else {
        panic!("expected one frame, got {events:?}");
    };
    assert_eq!(frame.data, data);
    assert_eq!(frame.data.len(), 1000);
}
