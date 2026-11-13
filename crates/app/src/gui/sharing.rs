//! [`SharingService`] backed by the real host service ([`HostHandle`]).

use anyhow::Result;

use crate::cli::HostOptions;
use crate::host::{HostEvent, HostHandle, HostObserver};

use super::ports::{AppEvent, Inbox, SharingControl, SharingService};

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
        let observer = Forwarder { inbox: self.inbox.clone(), generation };
        Ok(Box::new(HostHandle::start(options, observer)?))
    }
}

/// Forwards the service's events to the interface's queue.
struct Forwarder {
    inbox: Inbox,
    generation: u64,
}

impl HostObserver for Forwarder {
    fn on_event(&self, event: HostEvent) {
        self.inbox.send(AppEvent::Host { generation: self.generation, event });
    }
}

impl SharingControl for HostHandle {
    fn set_input_allowed(&self, allowed: bool) {
        HostHandle::set_input_allowed(self, allowed);
    }

    fn new_pin(&self) -> String {
        HostHandle::new_pin(self)
    }

    fn disconnect_viewer(&self) {
        HostHandle::disconnect_viewer(self);
    }

    fn stop(self: Box<Self>) {
        HostHandle::stop(*self);
    }
}
