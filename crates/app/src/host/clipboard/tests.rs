use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use protocol::control::MAX_CLIPBOARD_BYTES;
use tokio::sync::mpsc::UnboundedSender;

use super::*;
use crate::clipboard::ClipboardContent;
use crate::clipboard::fake::{FakeClipboard, link};
use crate::clipboard::fake_sharing::FakeSharing;
use crate::host::events::recording::recorder;

struct Fixture {
    clipboard: SessionClipboard<FakeClipboard>,
    copied: FakeClipboard,
    _announce: UnboundedSender<()>,
    sharing: watch::Sender<bool>,
    receive_files: watch::Sender<bool>,
    files: FakeSharing,
    events: Receiver<HostEvent>,
}

fn fixture(sharing: bool) -> Fixture {
    let (link, copied, announce) = link();
    let (sharing_tx, sharing_rx) = watch::channel(sharing);
    let (receive_tx, receive_rx) = watch::channel(true);
    let (observer, events) = recorder();
    let settings = ClipboardSettings {
        sharing: sharing_rx,
        receive_files: receive_rx,
    };
    Fixture {
        clipboard: SessionClipboard::with_link(Some(link), settings, EventSink::new(observer)),
        copied,
        _announce: announce,
        sharing: sharing_tx,
        receive_files: receive_tx,
        files: FakeSharing::default(),
        events,
    }
}

fn copy(fixture: &Fixture, text: &str) {
    fixture.copied.0.borrow_mut().content = Some(ClipboardContent::Text(text.into()));
}

fn copy_files(fixture: &Fixture, paths: &[&str]) {
    let paths = paths.iter().map(PathBuf::from).collect();
    fixture.copied.0.borrow_mut().content = Some(ClipboardContent::Files(paths));
}

fn written(fixture: &Fixture) -> Vec<String> {
    fixture.copied.0.borrow().written.clone()
}

async fn local_change(f: &mut Fixture, input_allowed: bool) -> Option<ControlMessage> {
    f.clipboard.local_change(input_allowed, &f.files).await
}

fn remote_files(f: &mut Fixture, input_allowed: bool) -> Received {
    let entries = vec![FileEntry::file("nota.txt", 4)];
    f.clipboard
        .remote_files(OfferId(1), entries, input_allowed, &f.files)
}

#[tokio::test]
async fn text_copied_on_the_host_goes_to_the_viewer() {
    // Remote control does not matter for what the host sends.
    for input_allowed in [true, false] {
        let mut f = fixture(true);
        copy(&f, "ciao");
        assert_eq!(
            local_change(&mut f, input_allowed).await,
            Some(ControlMessage::Clipboard("ciao".into()))
        );
    }
}

#[tokio::test]
async fn nothing_is_sent_while_the_setting_is_off() {
    let mut f = fixture(false);
    copy(&f, "segreto");
    assert_eq!(local_change(&mut f, true).await, None);
    // Turned on mid-session: the next change goes out.
    f.sharing.send(true).unwrap();
    assert_eq!(
        local_change(&mut f, true).await,
        Some(ControlMessage::Clipboard("segreto".into()))
    );
}

#[tokio::test]
async fn the_viewers_text_needs_the_setting_and_remote_control() {
    let mut f = fixture(true);
    assert_eq!(
        f.clipboard.remote_text("no", false).await,
        Received::Ignored
    );
    f.sharing.send(false).unwrap();
    assert_eq!(f.clipboard.remote_text("no", true).await, Received::Ignored);
    assert!(written(&f).is_empty());

    f.sharing.send(true).unwrap();
    assert_eq!(f.clipboard.remote_text("sì", true).await, Received::Applied);
    assert_eq!(written(&f), ["sì"]);
}

#[tokio::test]
async fn the_viewers_text_does_not_come_back() {
    let mut f = fixture(true);
    f.clipboard.remote_text("da remoto", true).await;
    assert_eq!(local_change(&mut f, true).await, None);
}

#[tokio::test]
async fn text_too_long_is_warned_about_once_and_not_sent() {
    let mut f = fixture(true);
    f.copied.0.borrow_mut().content = Some(ClipboardContent::TooLarge);
    assert_eq!(local_change(&mut f, true).await, None);
    assert_eq!(local_change(&mut f, true).await, None);
    assert_eq!(
        f.events.try_recv(),
        Ok(HostEvent::Warning(HostWarning::ClipboardTooLarge))
    );
    assert!(f.events.try_recv().is_err(), "only once");
}

#[tokio::test]
async fn too_long_text_from_the_viewer_is_ignored() {
    let mut f = fixture(true);
    let text = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
    assert_eq!(
        f.clipboard.remote_text(&text, true).await,
        Received::Ignored
    );
    assert!(written(&f).is_empty());
}

#[tokio::test]
async fn a_failing_clipboard_is_a_warning_not_an_error() {
    let mut f = fixture(true);
    f.copied.0.borrow_mut().fail_writes = true;
    assert_eq!(f.clipboard.remote_text("a", true).await, Received::Ignored);
    assert_eq!(f.clipboard.remote_text("b", true).await, Received::Ignored);
    assert_eq!(remote_files(&mut f, true), Received::Ignored);
    assert_eq!(
        f.events.try_recv(),
        Ok(HostEvent::Warning(HostWarning::ClipboardFailed(
            "denied".into()
        )))
    );
    assert!(f.events.try_recv().is_err(), "throttled");
}

#[tokio::test]
async fn without_a_listener_nothing_happens() {
    let (observer, _events) = recorder();
    let (_sharing_tx, sharing) = watch::channel(true);
    let (_receive_tx, receive_files) = watch::channel(true);
    let settings = ClipboardSettings {
        sharing,
        receive_files,
    };
    let mut clipboard =
        SessionClipboard::<FakeClipboard>::with_link(None, settings, EventSink::new(observer));
    let files = FakeSharing::default();
    assert_eq!(clipboard.local_change(true, &files).await, None);
    assert_eq!(clipboard.remote_text("a", true).await, Received::Ignored);
    let entries = vec![FileEntry::file("a", 1)];
    assert_eq!(
        clipboard.remote_files(OfferId(1), entries, true, &files),
        Received::Ignored
    );
    let waiting = tokio::time::timeout(Duration::from_millis(20), clipboard.changed());
    assert!(waiting.await.is_err());
}

#[tokio::test]
async fn files_copied_on_the_host_are_offered_and_withdrawn_by_the_next_copy() {
    let mut f = fixture(true);
    copy_files(&f, &["C:\\Foto", "C:\\nota.txt"]);
    assert_eq!(
        local_change(&mut f, false).await,
        None,
        "the list goes later"
    );
    assert_eq!(
        *f.files.offered.lock().unwrap(),
        [vec![
            PathBuf::from("C:\\Foto"),
            PathBuf::from("C:\\nota.txt")
        ]]
    );
    // The same files announced again: nothing new.
    assert_eq!(local_change(&mut f, false).await, None);
    assert_eq!(f.files.offered.lock().unwrap().len(), 1);
    assert_eq!(*f.files.withdrawn.lock().unwrap(), 0);
    // Text copied: the files cannot be pasted any more.
    copy(&f, "ciao");
    local_change(&mut f, false).await;
    assert_eq!(*f.files.withdrawn.lock().unwrap(), 1);
}

#[tokio::test]
async fn no_files_are_offered_while_the_clipboard_is_not_shared() {
    let mut f = fixture(false);
    copy_files(&f, &["C:\\a.txt"]);
    assert_eq!(local_change(&mut f, true).await, None);
    assert!(f.files.offered.lock().unwrap().is_empty());
    assert_eq!(
        *f.files.withdrawn.lock().unwrap(),
        1,
        "an earlier offer ends too"
    );
}

#[tokio::test]
async fn the_viewers_files_need_clipboard_control_and_reception() {
    let mut f = fixture(true);
    assert_eq!(remote_files(&mut f, false), Received::Ignored, "no control");
    f.receive_files.send(false).unwrap();
    assert_eq!(
        remote_files(&mut f, true),
        Received::Ignored,
        "reception off"
    );
    f.receive_files.send(true).unwrap();
    f.sharing.send(false).unwrap();
    assert_eq!(
        remote_files(&mut f, true),
        Received::Ignored,
        "clipboard off"
    );
    assert!(f.copied.0.borrow().offered.is_empty());

    f.sharing.send(true).unwrap();
    assert_eq!(remote_files(&mut f, true), Received::Applied);
    assert_eq!(f.copied.0.borrow().offered, [vec!["nota.txt".to_string()]]);
}

#[tokio::test]
async fn an_unsafe_list_from_the_viewer_never_reaches_the_clipboard() {
    let mut f = fixture(true);
    let entries = vec![FileEntry::file("../../Windows/evil.dll", 1)];
    assert_eq!(
        f.clipboard
            .remote_files(OfferId(1), entries, true, &f.files),
        Received::Ignored
    );
    assert!(f.copied.0.borrow().offered.is_empty());
}
