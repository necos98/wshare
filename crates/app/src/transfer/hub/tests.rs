//! Two hubs over a real loopback QUIC connection, their control messages
//! passed from one to the other by the test: pushes, names already taken,
//! refusals, cancellation, copy and paste.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use net::{ControlSender, HostEndpoint, Identity};
use protocol::control::{ControlMessage, PROTOCOL_VERSION};
use protocol::transfer::{TransferError, TransferMessage};
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::transfer::events::{Direction, Outcome};
use crate::transfer::{Chunk, TransferInfo, listing};

const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy)]
struct Allow {
    send: bool,
    accept: bool,
    serve: bool,
}

const ALL: Allow = Allow {
    send: true,
    accept: true,
    serve: true,
};

impl TransferPermissions for Allow {
    fn may_send(&self) -> bool {
        self.send
    }
    fn accept_push(&self) -> bool {
        self.accept
    }
    fn serve_clipboard(&self) -> bool {
        self.serve
    }
}

struct Side {
    hub: Option<TransferHub>,
    out: UnboundedReceiver<ControlMessage>,
    events: Receiver<TransferEvent>,
    seen: Vec<TransferEvent>,
    dir: tempfile::TempDir,
}

impl Side {
    fn hub(&self) -> &TransferHub {
        self.hub.as_ref().unwrap()
    }

    fn destination(&self) -> PathBuf {
        self.dir.path().join("wshare")
    }

    fn collect(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
    }

    fn outcome(&self) -> Option<Outcome> {
        self.seen.iter().find_map(|event| match event {
            TransferEvent::Finished { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
    }

    fn started(&self) -> Option<TransferInfo> {
        self.seen.iter().find_map(|event| match event {
            TransferEvent::Started(info) => Some(info.clone()),
            _ => None,
        })
    }
}

/// The two ends of a connection, each with its hub. The endpoint and the
/// control senders keep the connection open.
struct Pair {
    host: Side,
    viewer: Side,
    _keep: (HostEndpoint, ControlSender, ControlSender),
}

fn side(role: Role, files: net::FileChannel, allow: Allow) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let (tx, events) = channel();
    let observer = move |event| {
        let _ = tx.send(event);
    };
    let config = HubConfig {
        role,
        destination: dir.path().join("wshare"),
    };
    let (hub, out) = TransferHub::start(files, config, Arc::new(allow), Arc::new(observer));
    Side {
        hub: Some(hub),
        out,
        events,
        seen: Vec::new(),
        dir,
    }
}

async fn pair(host_allows: Allow, viewer_allows: Allow) -> Pair {
    let identity = Identity::generate("host").unwrap();
    let endpoint = HostEndpoint::bind((Ipv4Addr::LOCALHOST, 0).into(), &identity).unwrap();
    let addr = endpoint.local_addr().unwrap();
    let (accepted, viewer) = tokio::join!(endpoint.accept(), async {
        let session = net::connect(addr, Some(identity.fingerprint()), None)
            .await
            .unwrap();
        let (mut tx, _rx, _video, files) = session.split();
        tx.send(&ControlMessage::Hello {
            version: PROTOCOL_VERSION,
            viewer_name: "v".into(),
            pin: None,
        })
        .await
        .unwrap();
        (tx, files)
    });
    let (host_tx, mut host_rx, _video, host_files) = accepted.unwrap().split();
    host_rx.recv().await.unwrap();
    let (viewer_tx, viewer_files) = viewer;
    Pair {
        host: side(Role::Host, host_files, host_allows),
        viewer: side(Role::Viewer, viewer_files, viewer_allows),
        _keep: (endpoint, host_tx, viewer_tx),
    }
}

impl Pair {
    /// Passes the messages of each hub to the other until `done` holds.
    async fn run_until(&mut self, done: impl Fn(&Side, &Side) -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            self.host.collect();
            self.viewer.collect();
            if done(&self.host, &self.viewer) {
                return;
            }
            let pause = tokio::time::sleep(Duration::from_millis(5));
            tokio::select! {
                Some(message) = self.host.out.recv() => deliver(&self.viewer, message),
                Some(message) = self.viewer.out.recv() => deliver(&self.host, message),
                () = pause => {}
            }
            assert!(tokio::time::Instant::now() < deadline, "timed out");
        }
    }
}

fn deliver(to: &Side, message: ControlMessage) {
    let ControlMessage::Transfer(message) = message else {
        panic!("not a transfer message: {message:?}");
    };
    if let Some(hub) = &to.hub {
        hub.on_message(message);
    }
}

fn both_finished(host: &Side, viewer: &Side) -> bool {
    host.outcome().is_some() && viewer.outcome().is_some()
}

fn tree(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        found.push(relative);
        if path.is_dir() {
            walk(root, &path, found);
        }
    }
}

/// A folder `Foto` with two files, and a file `nota.txt`, in a temporary
/// folder of the sender.
fn picked(dir: &Path) -> Vec<PathBuf> {
    let foto = dir.join("Foto");
    std::fs::create_dir_all(foto.join("2024")).unwrap();
    std::fs::write(foto.join("2024").join("a.jpg"), vec![7u8; 300_000]).unwrap();
    std::fs::write(foto.join("b.jpg"), b"bb").unwrap();
    std::fs::write(dir.join("nota.txt"), b"ciao").unwrap();
    vec![foto, dir.join("nota.txt")]
}

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

/// The destination ends up holding nothing (the folder itself may stay).
async fn wait_until_empty(destination: &Path) {
    for _ in 0..200 {
        if tree(destination).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("left behind: {:?}", tree(destination));
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
