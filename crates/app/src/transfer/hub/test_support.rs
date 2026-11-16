//! What the hub's tests share: two hubs over a real loopback QUIC
//! connection, their control messages passed from one to the other by the
//! test, the permissions each side grants, and looks at the disk.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use net::{ControlSender, HostEndpoint, Identity};
use protocol::control::{ControlMessage, PROTOCOL_VERSION};
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::transfer::TransferInfo;
use crate::transfer::events::Outcome;

pub(super) const WAIT: Duration = Duration::from_secs(20);

/// Fixed permissions.
#[derive(Clone, Copy)]
pub(super) struct Allow {
    pub(super) send: bool,
    pub(super) accept: bool,
    pub(super) serve: bool,
    pub(super) paste: bool,
}

pub(super) const ALL: Allow = Allow {
    send: true,
    accept: true,
    serve: true,
    paste: true,
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
    fn paste_from_peer(&self) -> bool {
        self.paste
    }
}

/// Permissions to receive that can be taken back while the session lasts;
/// sending and serving are always allowed.
pub(super) struct Switches {
    accept: AtomicBool,
    paste: AtomicBool,
}

impl Switches {
    pub(super) fn all_on() -> Arc<Self> {
        Arc::new(Self {
            accept: AtomicBool::new(true),
            paste: AtomicBool::new(true),
        })
    }

    pub(super) fn set_accept(&self, on: bool) {
        self.accept.store(on, Ordering::Relaxed);
    }

    pub(super) fn set_paste(&self, on: bool) {
        self.paste.store(on, Ordering::Relaxed);
    }
}

impl TransferPermissions for Switches {
    fn may_send(&self) -> bool {
        true
    }
    fn accept_push(&self) -> bool {
        self.accept.load(Ordering::Relaxed)
    }
    fn serve_clipboard(&self) -> bool {
        true
    }
    fn paste_from_peer(&self) -> bool {
        self.paste.load(Ordering::Relaxed)
    }
}

pub(super) struct Side {
    pub(super) hub: Option<TransferHub>,
    pub(super) out: UnboundedReceiver<ControlMessage>,
    events: Receiver<TransferEvent>,
    pub(super) seen: Vec<TransferEvent>,
    dir: tempfile::TempDir,
}

impl Side {
    pub(super) fn hub(&self) -> &TransferHub {
        self.hub.as_ref().unwrap()
    }

    pub(super) fn destination(&self) -> PathBuf {
        self.dir.path().join("wshare")
    }

    pub(super) fn collect(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
    }

    pub(super) fn outcome(&self) -> Option<Outcome> {
        self.seen.iter().find_map(|event| match event {
            TransferEvent::Finished { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
    }

    pub(super) fn started(&self) -> Option<TransferInfo> {
        self.seen.iter().find_map(|event| match event {
            TransferEvent::Started(info) => Some(info.clone()),
            _ => None,
        })
    }

    /// Whether some of a transfer arrived or left.
    pub(super) fn moved(&self) -> bool {
        self.seen
            .iter()
            .any(|e| matches!(e, TransferEvent::Progress { done_bytes, .. } if *done_bytes > 0))
    }
}

/// The two ends of a connection, each with its hub. The endpoint and the
/// control senders keep the connection open.
pub(super) struct Pair {
    pub(super) host: Side,
    pub(super) viewer: Side,
    _keep: (HostEndpoint, ControlSender, ControlSender),
}

fn side(role: Role, files: net::FileChannel, permissions: Arc<dyn TransferPermissions>) -> Side {
    let dir = tempfile::tempdir().unwrap();
    let (tx, events) = channel();
    let observer = move |event| {
        let _ = tx.send(event);
    };
    let config = HubConfig {
        role,
        destination: dir.path().join("wshare"),
    };
    let (hub, out) = TransferHub::start(files, config, permissions, Arc::new(observer));
    Side {
        hub: Some(hub),
        out,
        events,
        seen: Vec::new(),
        dir,
    }
}

pub(super) async fn pair(host_allows: Allow, viewer_allows: Allow) -> Pair {
    pair_with(Arc::new(host_allows), Arc::new(viewer_allows)).await
}

pub(super) async fn pair_with(
    host_allows: Arc<dyn TransferPermissions>,
    viewer_allows: Arc<dyn TransferPermissions>,
) -> Pair {
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
    pub(super) async fn run_until(&mut self, done: impl Fn(&Side, &Side) -> bool) {
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

pub(super) fn deliver(to: &Side, message: ControlMessage) {
    let ControlMessage::Transfer(message) = message else {
        panic!("not a transfer message: {message:?}");
    };
    if let Some(hub) = &to.hub {
        hub.on_message(message);
    }
}

/// The next message `side` has for the other PC, waited for.
pub(super) async fn next_out(side: &mut Side) -> TransferMessage {
    let message = tokio::time::timeout(WAIT, side.out.recv())
        .await
        .expect("timed out")
        .expect("the hub is gone");
    let ControlMessage::Transfer(message) = message else {
        panic!("not a transfer message: {message:?}");
    };
    message
}

pub(super) fn both_finished(host: &Side, viewer: &Side) -> bool {
    host.outcome().is_some() && viewer.outcome().is_some()
}

pub(super) fn tree(root: &Path) -> Vec<String> {
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

/// The destination ends up holding nothing (the folder itself may stay).
pub(super) async fn wait_until_empty(destination: &Path) {
    for _ in 0..200 {
        if tree(destination).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("left behind: {:?}", tree(destination));
}

/// A folder `Foto` with two files, and a file `nota.txt`, in a temporary
/// folder of the sender.
pub(super) fn picked(dir: &Path) -> Vec<PathBuf> {
    let foto = dir.join("Foto");
    std::fs::create_dir_all(foto.join("2024")).unwrap();
    std::fs::write(foto.join("2024").join("a.jpg"), vec![7u8; 300_000]).unwrap();
    std::fs::write(foto.join("b.jpg"), b"bb").unwrap();
    std::fs::write(dir.join("nota.txt"), b"ciao").unwrap();
    vec![foto, dir.join("nota.txt")]
}

/// A file of `size` bytes in `dir`: big enough to be stopped while it moves.
pub(super) fn big_file(dir: &Path, size: usize) -> PathBuf {
    let big = dir.join("grande.bin");
    std::fs::write(&big, vec![1u8; size]).unwrap();
    big
}
