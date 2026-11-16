//! Why a transfer failed stays typed from where it happens to what the
//! front-ends hear: a file that changed, a file that is not the one asked
//! for, files that cannot be listed, a cause told by the other PC.

use std::path::PathBuf;

use protocol::transfer::{FileEntry, TransferError, TransferId, TransferMessage};

use super::test_support::*;
use super::*;
use crate::transfer::events::{Failure, Outcome};
use crate::transfer::listing;

/// The index, in the list of [`picked`], of `nota.txt` (4 bytes).
const NOTE: u32 = 4;

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_got_shorter_since_it_was_copied_fails_as_such() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let listing = listing::list(&picked(source.path())).unwrap();
    let entry = listing.entries[NOTE as usize].clone();
    std::fs::write(&listing.sources[NOTE as usize], b"").unwrap();
    let offer = pair.host.hub().offer_clipboard(listing);
    let _chunks = pair
        .viewer
        .hub()
        .paste_link()
        .fetch(offer, NOTE, entry)
        .unwrap();
    pair.run_until(|host, _| host.outcome().is_some()).await;
    assert_eq!(
        pair.host.outcome(),
        Some(Outcome::Failed(Failure::Cause(TransferError::FileShrank)))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_file_that_is_not_the_one_asked_for_fails_as_such() {
    let mut pair = pair(ALL, ALL).await;
    let source = tempfile::tempdir().unwrap();
    let listing = listing::list(&picked(source.path())).unwrap();
    let mut asked = listing.entries[NOTE as usize].clone();
    asked.size += 1;
    let offer = pair.host.hub().offer_clipboard(listing);
    let mut chunks = pair
        .viewer
        .hub()
        .paste_link()
        .fetch(offer, NOTE, asked)
        .unwrap();
    pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
    assert_eq!(
        pair.viewer.outcome(),
        Some(Outcome::Failed(Failure::Cause(TransferError::NotAsked)))
    );
    assert_eq!(chunks.recv().await, None, "the read fails, without an end");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_that_cannot_be_listed_fail_with_why() {
    let mut pair = pair(ALL, ALL).await;
    let missing = PathBuf::from("Z:\\non\\esiste");
    pair.viewer
        .hub()
        .command(TransferCommand::Send(vec![missing.clone()]));
    pair.run_until(|_, viewer| viewer.outcome().is_some()).await;
    let Some(Outcome::Failed(Failure::Listing(ListingError::Unreadable { path, .. }))) =
        pair.viewer.outcome()
    else {
        panic!("not a listing failure: {:?}", pair.viewer.outcome());
    };
    assert_eq!(path, missing);
    assert!(pair.viewer.out.try_recv().is_err(), "no offer went out");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_that_cannot_be_offered_say_why() {
    let pair = pair(ALL, ALL).await;
    let (tx, rx) = std::sync::mpsc::channel();
    pair.host
        .hub()
        .offer_clipboard_files(Vec::new(), move |error| {
            let _ = tx.send(error);
        });
    assert_eq!(rx.recv_timeout(WAIT), Ok(ListingError::Nothing));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_of_a_file_told_by_the_other_pc_stays_typed_and_clean() {
    let reasons = [
        (TransferError::Incomplete, TransferError::Incomplete),
        (
            TransferError::StreamFailed("rete\r\npersa".into()),
            TransferError::StreamFailed("retepersa".into()),
        ),
        (
            TransferError::CannotReadFile("a\u{1b}b".into()),
            TransferError::CannotReadFile("ab".into()),
        ),
        (
            TransferError::CannotCreateFile("a\nb".into()),
            TransferError::CannotCreateFile("ab".into()),
        ),
        (
            TransferError::CannotWriteFile("a\tb".into()),
            TransferError::CannotWriteFile("ab".into()),
        ),
        (
            TransferError::CannotSaveFile("a\rb".into()),
            TransferError::CannotSaveFile("ab".into()),
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
        assert_eq!(
            pair.viewer.outcome(),
            Some(Outcome::Failed(Failure::Cause(expected)))
        );
    }
}
