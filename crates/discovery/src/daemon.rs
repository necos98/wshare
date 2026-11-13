//! Thin I/O wrappers around the `mdns-sd` daemon. All decisions live in the pure modules.

use std::time::{Duration, Instant};

use anyhow::Context;
use mdns_sd::{IfKind, Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::event::EventTranslator;
use crate::{
    DiscoveryEvent, HostAnnouncement, SERVICE_TYPE, computer_name, host_name, instance_name,
};

/// How long `Drop` waits for the goodbye packet / daemon shutdown to complete.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

/// Starts an mDNS daemon on all interfaces except loopback.
fn new_daemon() -> anyhow::Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    daemon
        .disable_interface(vec![IfKind::LoopbackV4, IfKind::LoopbackV6])
        .context("disabling loopback interfaces for mDNS")?;
    Ok(daemon)
}

fn shutdown(daemon: &ServiceDaemon) {
    if let Ok(status) = daemon.shutdown() {
        let _ = status.recv_timeout(SHUTDOWN_WAIT);
    }
}

/// Announces this host on the LAN for as long as it is alive.
///
/// Addresses are discovered automatically from the non-loopback interfaces and kept up to
/// date when they change (DHCP renewals, Wi-Fi switches). Dropping it sends a goodbye
/// packet (so viewers get [`DiscoveryEvent::Lost`] promptly) and stops the daemon.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// Validates `announcement` and starts announcing it.
    pub fn start(announcement: &HostAnnouncement) -> anyhow::Result<Self> {
        announcement.validate()?;
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name(&announcement.display_name, &announcement.id),
            &host_name(&computer_name(), &announcement.id),
            (),
            announcement.port,
            announcement.to_txt(),
        )
        .context("building mDNS service info")?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        let daemon = new_daemon()?;
        if let Err(err) = daemon.register(info) {
            shutdown(&daemon);
            return Err(err).context("registering mDNS service");
        }
        Ok(Self { daemon, fullname })
    }

    /// The DNS-SD fullname being announced (e.g. `"DESK-a1b2._wshare._udp.local."`).
    pub fn fullname(&self) -> &str {
        &self.fullname
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        if let Ok(status) = self.daemon.unregister(&self.fullname) {
            let _ = status.recv_timeout(SHUTDOWN_WAIT);
        }
        shutdown(&self.daemon);
    }
}

/// Watches the LAN for wshare hosts. Stops browsing when dropped.
///
/// Typical use: poll [`Browser::try_iter`] once per UI frame (or block on
/// [`Browser::recv_timeout`] in a worker thread) and feed events into a [`crate::HostList`].
pub struct Browser {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    translator: EventTranslator,
}

impl Browser {
    /// Starts browsing for [`SERVICE_TYPE`].
    pub fn start() -> anyhow::Result<Self> {
        let daemon = new_daemon()?;
        let events = match daemon.browse(SERVICE_TYPE) {
            Ok(events) => events,
            Err(err) => {
                shutdown(&daemon);
                return Err(err).context("starting mDNS browse");
            }
        };
        Ok(Self {
            daemon,
            events,
            translator: EventTranslator::default(),
        })
    }

    /// Waits up to `timeout` for the next event. Returns `None` on timeout or if the daemon
    /// stopped.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<DiscoveryEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            let event = self.events.recv_deadline(deadline).ok()?;
            if let Some(event) = self.translator.translate(event) {
                return Some(event);
            }
        }
    }

    /// Returns the next pending event without blocking.
    pub fn try_recv(&mut self) -> Option<DiscoveryEvent> {
        loop {
            let event = self.events.try_recv().ok()?;
            if let Some(event) = self.translator.translate(event) {
                return Some(event);
            }
        }
    }

    /// Drains all pending events without blocking.
    pub fn try_iter(&mut self) -> impl Iterator<Item = DiscoveryEvent> + '_ {
        std::iter::from_fn(move || self.try_recv())
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        shutdown(&self.daemon);
    }
}
