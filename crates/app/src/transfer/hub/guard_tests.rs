//! What a peer cannot get past: the limit of transfers at once (proposals
//! being planned included), permissions taken back while files move, and
//! sizes that add up beyond `u64::MAX`.

use std::collections::BTreeSet;
use std::time::Duration;

use protocol::transfer::{FileEntry, TransferError, TransferId, TransferMessage};

use super::test_support::*;
use super::*;
use crate::transfer::events::{Failure, Outcome};
use crate::transfer::registry::MAX_ACTIVE_TRANSFERS;
use crate::transfer::{Chunk, listing};

const FLOOD: u32 = 100;
const BIG: usize = 256 * 1024 * 1024;

/// A folder holding a folder holding a file: `d<n>/sub/f`.
fn nested(n: u32) -> Vec<FileEntry> {
    vec![
        FileEntry::folder(format!("d{n}")),
        FileEntry::folder(format!("d{n}/sub")),
        FileEntry::file(format!("d{n}/sub/f"), 1),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_offers_stays_within_the_limit_and_leaves_no_folders() {
    let mut pair = pair(ALL, ALL).await;
    // Ids the viewer may pick (odd), all proposed before any is planned.
    for n in 0..FLOOD {
        let id = TransferId(1 + 2 * n);
        pair.host.hub().on_message(TransferMessage::Offer {
            id,
            entries: nested(n),
        });
    }
    let mut accepted = BTreeSet::new();
    let mut refused = 0;
    for _ in 0..FLOOD {
        match next_out(&mut pair.host).await {
            TransferMessage::Accept { id } => {
                accepted.insert(format!("d{}", (id.0 - 1) / 2));
            }
            TransferMessage::Failed {
                reason: TransferError::TooManyTransfers,
                ..
            } => refused += 1,
            other => panic!("unexpected answer: {other:?}"),
        }
    }
    assert!(!accepted.is_empty());
    assert!(accepted.len() <= MAX_ACTIVE_TRANSFERS, "{}", accepted.len());
    assert_eq!(accepted.len() + refused, FLOOD as usize);

    let destination = pair.host.destination();
    let roots: BTreeSet<String> = tree(&destination)
        .into_iter()
        .filter(|path| !path.contains('/'))
        .collect();
    assert_eq!(roots, accepted, "only the accepted pushes made folders");

    // The session ends before any file came: nothing stays.
    pair.host.hub = None;
    wait_until_empty(&destination).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_reception_off_stops_a_push_under_way() {
    let switches = Switches::all_on();
    let mut pair = pair_with(switches.clone(), Arc::new(ALL)).await;
    let source = tempfile::tempdir().unwrap();
    let big = big_file(source.path(), BIG);
    pair.viewer.hub().command(TransferCommand::Send(vec![big]));
    pair.run_until(|host, _| host.moved()).await;

    switches.set_accept(false);
    pair.host.hub().enforce_permissions();
    pair.run_until(both_finished).await;
    assert_eq!(
        pair.host.outcome(),
        Some(Outcome::Failed(Failure::NoLongerAllowed))
    );
    assert_eq!(
        pair.viewer.outcome(),
        Some(Outcome::Failed(Failure::Cause(TransferError::NotAllowed)))
    );
    wait_until_empty(&pair.host.destination()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn files_arriving_after_reception_was_turned_off_are_refused() {
    let switches = Switches::all_on();
    let mut pair = pair_with(switches.clone(), Arc::new(ALL)).await;
    let source = tempfile::tempdir().unwrap();
    pair.viewer
        .hub()
        .command(TransferCommand::Send(picked(source.path())));
    let offer = next_out(&mut pair.viewer).await;
    assert!(matches!(offer, TransferMessage::Offer { .. }));
    pair.host.hub().on_message(offer);
    let accept = next_out(&mut pair.host).await;
    assert!(matches!(accept, TransferMessage::Accept { .. }));

    // Taken back without the session noticing: the streams find out.
    switches.set_accept(false);
    pair.viewer.hub().on_message(accept);
    pair.run_until(both_finished).await;
    assert_eq!(
        pair.host.outcome(),
        Some(Outcome::Failed(Failure::NoLongerAllowed))
    );
    assert!(matches!(pair.viewer.outcome(), Some(Outcome::Failed(_))));
    wait_until_empty(&pair.host.destination()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_refused_while_planned_leaves_nothing() {
    let switches = Switches::all_on();
    let mut pair = pair_with(switches.clone(), Arc::new(ALL)).await;
    pair.host.hub().on_message(TransferMessage::Offer {
        id: TransferId(1),
        entries: nested(0),
    });
    // Taken back at once: whether planning was over by then (an Accept
    // went first) or not, no folder stays and the viewer is told.
    switches.set_accept(false);
    pair.host.hub().enforce_permissions();
    let answer = match next_out(&mut pair.host).await {
        TransferMessage::Accept { .. } => next_out(&mut pair.host).await,
        other => other,
    };
    assert!(
        matches!(
            answer,
            TransferMessage::Failed {
                reason: TransferError::NotAllowed,
                ..
            }
        ),
        "{answer:?}"
    );
    wait_until_empty(&pair.host.destination()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_peers_files_can_no_longer_be_pasted_once_not_allowed() {
    let switches = Switches::all_on();
    let mut pair = pair_with(switches.clone(), Arc::new(ALL)).await;
    let source = tempfile::tempdir().unwrap();
    let listing = listing::list(&[big_file(source.path(), BIG)]).unwrap();
    let entry = listing.entries[0].clone();
    let offer = pair.viewer.hub().offer_clipboard(listing);
    let link = pair.host.hub().paste_link();
    // Not read: the paste stays under way.
    let mut chunks = link.fetch(offer, 0, entry.clone()).unwrap();
    pair.run_until(|host, _| host.started().is_some()).await;

    switches.set_paste(false);
    pair.host.hub().enforce_permissions();
    pair.run_until(both_finished).await;
    assert_eq!(
        pair.host.outcome(),
        Some(Outcome::Failed(Failure::NoLongerAllowed))
    );
    while let Some(chunk) = chunks.recv().await {
        assert_ne!(chunk, Chunk::End, "the paste must not complete");
    }

    // A paste asked for afterwards fails at once, asking nothing.
    let mut again = link.fetch(offer, 0, entry).unwrap();
    let refused = tokio::time::timeout(WAIT, again.recv()).await.unwrap();
    assert_eq!(refused, None);
    tokio::time::sleep(Duration::from_millis(50)).await;
    while let Ok(message) = pair.host.out.try_recv() {
        assert!(
            !matches!(
                message,
                protocol::control::ControlMessage::Transfer(TransferMessage::Request { .. })
            ),
            "asked for a file no longer allowed"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sizes_adding_up_beyond_u64_are_refused_as_invalid() {
    let mut pair = pair(ALL, ALL).await;
    pair.host.hub().on_message(TransferMessage::Offer {
        id: TransferId(1),
        entries: vec![FileEntry::file("a", u64::MAX), FileEntry::file("b", 1)],
    });
    let answer = next_out(&mut pair.host).await;
    assert!(
        matches!(
            answer,
            TransferMessage::Failed {
                reason: TransferError::Invalid,
                ..
            }
        ),
        "{answer:?}"
    );
    pair.host.collect();
    assert!(pair.host.started().is_none());
    assert!(tree(&pair.host.destination()).is_empty());
}
