//! [`SharingService`] backed by the real host service ([`HostHandle`]).

use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use anyhow::Result;
use net::Fingerprint;
use protocol::transfer::TransferId;

use crate::access_mode::AccessMode;
use crate::host::{ApprovalAnswer, ApprovalId, HostEvent, HostHandle, HostObserver};
use crate::options::HostOptions;

use super::ports::{AppEvent, Inbox, PendingStop, SharingControl, SharingService};

pub struct HostService {
    inbox: Inbox,
}

impl HostService {
    pub fn new(inbox: Inbox) -> Self {
        Self { inbox }
    }
}

impl SharingService for HostService {
    fn start(&mut self, options: HostOptions, generation: u64) -> Result<Box<dyn SharingControl>> {
        let observer = Forwarder {
            inbox: self.inbox.clone(),
            generation,
        };
        let handle = HostHandle::start(options, observer)?;
        Ok(Box::new(RunningHost {
            handle,
            inbox: self.inbox.clone(),
            generation,
        }))
    }
}

/// Forwards the service's events to the interface's queue.
struct Forwarder {
    inbox: Inbox,
    generation: u64,
}

impl HostObserver for Forwarder {
    fn on_event(&self, event: HostEvent) {
        self.inbox.send(AppEvent::Host {
            generation: self.generation,
            event,
        });
    }
}

/// A running service and where to report the end of a background stop.
struct RunningHost {
    handle: HostHandle,
    inbox: Inbox,
    generation: u64,
}

impl SharingControl for RunningHost {
    fn set_input_allowed(&self, allowed: bool) {
        self.handle.set_input_allowed(allowed);
    }

    fn set_clipboard_shared(&self, shared: bool) {
        self.handle.set_clipboard_shared(shared);
    }

    fn set_receive_files(&self, receive: bool) {
        self.handle.set_receive_files(receive);
    }

    fn send_files(&self, picked: Vec<PathBuf>) -> bool {
        self.handle.send_files(picked)
    }

    fn cancel_transfer(&self, id: Option<TransferId>) {
        self.handle.cancel_transfer(id);
    }

    fn set_access_mode(&self, access: AccessMode) {
        self.handle.set_access_mode(access);
    }

    fn answer_approval(&self, id: ApprovalId, answer: ApprovalAnswer) {
        self.handle.answer_approval(id, answer);
    }

    fn new_pin(&self) -> String {
        self.handle.new_pin()
    }

    fn disconnect_viewer(&self) {
        self.handle.disconnect_viewer();
    }

    fn forget_device(&self, fingerprint: &Fingerprint) {
        self.handle.forget_device(fingerprint);
    }

    fn stop(self: Box<Self>) {
        self.handle.stop();
    }

    fn stop_in_background(self: Box<Self>) -> Box<dyn PendingStop> {
        let Self {
            handle,
            inbox,
            generation,
        } = *self;
        let done = inbox.clone();
        let stopper = move || {
            handle.stop();
            done.send(AppEvent::SharingStopped { generation });
        };
        let thread = thread::Builder::new()
            .name("wshare-host-stop".into())
            .spawn(stopper);
        if thread.is_err() {
            // The closure, and the handle in it, was dropped with the error;
            // dropping a handle stops the service and waits, so it is over.
            inbox.send(AppEvent::SharingStopped { generation });
        }
        Box::new(StopThread(thread.ok()))
    }
}

/// The thread running a background stop (`None`: it was done inline).
struct StopThread(Option<JoinHandle<()>>);

impl PendingStop for StopThread {
    fn wait(self: Box<Self>) {
        if let Some(thread) = self.0 {
            let _ = thread.join();
        }
    }
}
