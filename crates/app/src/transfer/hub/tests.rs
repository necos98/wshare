//! Two hubs passing pushes, names already taken, refusals, cancellation,
//! copy and paste (see [`super::test_support`]).

use std::path::PathBuf;
use std::time::Duration;

use protocol::control::ControlMessage;
use protocol::transfer::{FileEntry, TransferError, TransferId, TransferMessage};

use super::test_support::*;
use super::*;
use crate::transfer::events::{Direction, Failure, Outcome};
use crate::transfer::shared::MAX_PEER_REASON_CHARS;
use crate::transfer::{Chunk, listing};

#[tokio::test(flavor = "multi_thread")]
async fn a_pushed_folder_arrives_with_its_structure() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    pair.viewer
        .hub()
        .command(TransferCommand::Send(picked(source.path())));
    pair.run_until(both_finished).await;

    let destination = pair.host.destination();
    assert_eq!(
        pair.host.outcome(),
        Some(Outcome::Completed {
            folder: Some(destination.clone())
        })
    );
    assert_eq!(
        pair.viewer.outcome(),
        Some(Outcome::Completed { folder: None })
    );
    assert_eq!(
        tree(&destination),
        [
            "Foto",
            "Foto/2024",
            "Foto/2024/a.jpg",
            "Foto/b.jpg",
            "nota.txt"
        ]
    );
    assert_eq!(
        std::fs::read(destination.join("Foto/2024/a.jpg")).unwrap(),
        vec![7u8; 300_000]
    );
    let info = pair.host.started().unwrap();
    assert_eq!((info.direction, info.files), (Direction::Receiving, 3));
    assert_eq!(info.total_bytes, 300_006);
}

#[tokio::test(flavor = "multi_thread")]
async fn names_already_taken_get_a_number() {
    let mut pair = pair(ALL, ALL).await;
    let destination = pair.viewer.destination();
    std::fs::create_dir_all(destination.join("Foto")).unwrap();
    std::fs::write(destination.join("nota.txt"), b"mia").unwrap();
    let source = tempfile::tempdir().unwrap();
    pair.host
        .hub()
        .command(TransferCommand::Send(picked(source.path())));
    pair.run_until(both_finished).await;

    assert!(matches!(
        pair.viewer.outcome(),
        Some(Outcome::Completed { .. })
    ));
    assert_eq!(
        tree(&destination),
        [
            "Foto",
            "Foto (1)",
            "Foto (1)/2024",
            "Foto (1)/2024/a.jpg",
            "Foto (1)/b.jpg",
            "nota (1).txt",
            "nota.txt"
        ]
    );
    assert_eq!(std::fs::read(destination.join("nota.txt")).unwrap(), b"mia");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_receiver_that_does_not_accept_files_refuses_them() {
    let refuse = Allow {
        accept: false,
        ..ALL
    };
    let mut pair = pair(refuse, ALL).await;
    let source = tempfile::tempdir().unwrap();
    pair.viewer
        .hub()
        .command(TransferCommand::Send(picked(source.path())));
    pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
    assert_eq!(
        pair.viewer.outcome(),
        Some(Outcome::Refused(TransferError::NotAllowed))
    );
    assert!(pair.host.started().is_none(), "nothing began on the host");
    assert!(!pair.host.destination().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sender_that_may_not_send_does_not_propose() {
    let quiet = Allow { send: false, ..ALL };
    let mut pair = pair(ALL, quiet).await;
    let source = tempfile::tempdir().unwrap();
    pair.viewer
        .hub()
        .command(TransferCommand::Send(picked(source.path())));
    pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
    assert_eq!(
        pair.viewer.outcome(),
        Some(Outcome::Refused(TransferError::NotAllowed))
    );
    assert!(pair.viewer.out.try_recv().is_err(), "no offer went out");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_transfer_leaves_no_part_file() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let big = source.path().join("grande.bin");
    std::fs::write(&big, vec![1u8; 256 * 1024 * 1024]).unwrap();
    pair.viewer.hub().command(TransferCommand::Send(vec![big]));
    // Cancelled on the receiver once some of it arrived.
    pair.run_until(|host, _| {
        host.seen
            .iter()
            .any(|e| matches!(e, TransferEvent::Progress { done_bytes, .. } if *done_bytes > 0))
    })
    .await;
    pair.host.hub().command(TransferCommand::CancelAll);
    pair.run_until(both_finished).await;
    assert_eq!(
        pair.host.outcome(),
        Some(Outcome::Cancelled { by_peer: false })
    );
    assert_eq!(
        pair.viewer.outcome(),
        Some(Outcome::Cancelled { by_peer: true })
    );
    wait_until_empty(&pair.host.destination()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_ends_interrupts_its_transfers_and_cleans_up() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let big = source.path().join("grande.bin");
    std::fs::write(&big, vec![1u8; 256 * 1024 * 1024]).unwrap();
    pair.host.hub().command(TransferCommand::Send(vec![big]));
    pair.run_until(|_, viewer| {
        viewer
            .seen
            .iter()
            .any(|e| matches!(e, TransferEvent::Progress { done_bytes, .. } if *done_bytes > 0))
    })
    .await;
    pair.viewer.hub = None;
    pair.viewer.collect();
    assert_eq!(pair.viewer.outcome(), Some(Outcome::Interrupted));
    wait_until_empty(&pair.viewer.destination()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_file_is_fetched_from_the_clipboard_offer() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let listing = listing::list(&picked(source.path())).unwrap();
    let entry = listing.entries[2].clone();
    assert_eq!(entry.path, "Foto/2024/a.jpg");
    let offer = pair.host.hub().offer_clipboard(listing);
    let mut chunks = pair
        .viewer
        .hub()
        .paste_link()
        .fetch(offer, 2, entry)
        .unwrap();

    let mut bytes = Vec::new();
    let mut ended = false;
    let deadline = tokio::time::Instant::now() + WAIT;
    while !ended {
        tokio::select! {
            chunk = chunks.recv() => match chunk {
                Some(Chunk::Data(data)) => bytes.extend_from_slice(&data),
                Some(Chunk::End) => ended = true,
                None => panic!("the paste failed"),
            },
            () = pair.run_until(|_, _| false) => {}
            () = tokio::time::sleep_until(deadline) => panic!("timed out"),
        }
    }
    assert_eq!(bytes, vec![7u8; 300_000]);
    pair.run_until(both_finished).await;
    assert_eq!(
        pair.viewer.outcome(),
        Some(Outcome::Completed { folder: None })
    );
    assert_eq!(
        pair.host.outcome(),
        Some(Outcome::Completed { folder: None })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paste_of_files_no_longer_offered_fails_cleanly() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let listing = listing::list(&picked(source.path())).unwrap();
    let entry = listing.entries[4].clone();
    let offer = pair.host.hub().offer_clipboard(listing);
    pair.host.hub().withdraw_clipboard();
    let mut chunks = pair
        .viewer
        .hub()
        .paste_link()
        .fetch(offer, 4, entry)
        .unwrap();
    pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
    assert!(matches!(pair.viewer.outcome(), Some(Outcome::Failed(_))));
    assert_eq!(chunks.recv().await, None, "the read fails, without an end");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_stopped_sharing_its_clipboard_serves_nothing() {
    let closed = Allow {
        serve: false,
        ..ALL
    };
    let mut pair = pair(closed, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let listing = listing::list(&picked(source.path())).unwrap();
    let entry = listing.entries[4].clone();
    let offer = pair.host.hub().offer_clipboard(listing);
    let mut chunks = pair
        .viewer
        .hub()
        .paste_link()
        .fetch(offer, 4, entry)
        .unwrap();
    pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
    assert_eq!(chunks.recv().await, None);
    assert!(pair.host.started().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn copied_files_are_announced_unless_the_clipboard_changed_meanwhile() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let picked = picked(source.path());
    pair.host
        .hub()
        .offer_clipboard_files(picked.clone(), |e| panic!("{e}"));
    let announced = tokio::time::timeout(WAIT, pair.host.out.recv())
        .await
        .unwrap();
    let Some(ControlMessage::Transfer(TransferMessage::ClipboardFiles { entries, .. })) = announced
    else {
        panic!("not an announcement: {announced:?}");
    };
    assert_eq!(entries.len(), 5);

    // Copied, then something else copied before the walk was over.
    pair.host
        .hub()
        .offer_clipboard_files(picked, |e| panic!("{e}"));
    pair.host.hub().withdraw_clipboard();
    let late = tokio::time::timeout(Duration::from_millis(300), pair.host.out.recv()).await;
    assert!(late.is_err(), "an outdated list is not announced");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_that_cannot_be_offered_are_reported() {
    let pair = pair(ALL, ALL).await;
    let (tx, rx) = std::sync::mpsc::channel();
    pair.host
        .hub()
        .offer_clipboard_files(vec![PathBuf::from("Z:\\non\\esiste")], move |e| {
            let _ = tx.send(e.to_string());
        });
    assert!(rx.recv_timeout(WAIT).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_naming_unknown_transfers_are_ignored() {
    let mut pair = pair(ALL, ALL).await;
    let id = protocol::transfer::TransferId(41);
    for message in [
        TransferMessage::Accept { id },
        TransferMessage::Completed { id },
        TransferMessage::Cancel { id },
        TransferMessage::Failed {
            id,
            reason: TransferError::Invalid,
        },
    ] {
        pair.host.hub().on_message(message);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    pair.host.collect();
    assert!(pair.host.seen.is_empty());
    assert!(pair.host.out.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_told_by_the_other_pc_is_kept_on_one_short_line() {
    let mut pair = pair(ALL, ALL).await;
    // An id the host may pick (even), offered to the viewer, which accepts.
    let id = TransferId(8);
    pair.viewer.hub().on_message(TransferMessage::Offer {
        id,
        entries: vec![FileEntry::file("a.txt", 1)],
    });
    pair.run_until(|_, viewer| viewer.started().is_some()).await;
    let forged = format!(
        "x\r\n@wshare:transfer-ended:8:done:\\\\evil\\share\u{1b}{}",
        "y".repeat(10 * MAX_PEER_REASON_CHARS)
    );
    pair.viewer.hub().on_message(TransferMessage::Failed {
        id,
        reason: TransferError::StreamFailed(forged),
    });
    pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
    let Some(Outcome::Failed(Failure::Cause(TransferError::StreamFailed(detail)))) =
        pair.viewer.outcome()
    else {
        panic!("not a failure: {:?}", pair.viewer.outcome());
    };
    assert!(
        !detail.contains(char::is_control),
        "a control character is left"
    );
    assert_eq!(detail.chars().count(), MAX_PEER_REASON_CHARS);
    assert!(detail.starts_with("x@wshare:transfer-ended:8:done:"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_known_failure_told_by_the_other_pc_stays_typed_and_its_text_is_cleaned() {
    let reasons = [
        (
            TransferError::TooManyTransfers,
            Failure::Cause(TransferError::TooManyTransfers),
        ),
        (
            TransferError::CannotCreateFolders("a\r\nb".into()),
            Failure::Cause(TransferError::CannotCreateFolders("ab".into())),
        ),
        (
            TransferError::PlanningFailed("x\u{1b}y".into()),
            Failure::Cause(TransferError::PlanningFailed("xy".into())),
        ),
    ];
    for (reason, expected) in reasons {
        let mut pair = pair(ALL, ALL).await;
        let id = TransferId(8);
        pair.viewer.hub().on_message(TransferMessage::Offer {
            id,
            entries: vec![FileEntry::file("a.txt", 1)],
        });
        pair.run_until(|_, viewer| viewer.started().is_some()).await;
        pair.viewer
            .hub()
            .on_message(TransferMessage::Failed { id, reason });
        pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
        assert_eq!(pair.viewer.outcome(), Some(Outcome::Failed(expected)));
    }
}
