//! The sharing service's lifetime: a dedicated thread with its own tokio
//! runtime, synchronous setup reporting, and an orderly stop.
//!
//! Own thread + own runtime so any front-end can host it: the CLI just
//! blocks, the GUI keeps its event loop free. Setup happens on that thread
//! (the QUIC endpoint must be created inside its runtime) but `start` waits
//! for its outcome, so "port in use" or "no such monitor" is an `Err` the
//! caller can show immediately rather than an event racing the UI.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};
use capture::MonitorInfo;
use discovery::{Advertiser, HostAnnouncement, computer_name};
use net::{HostEndpoint, Identity};
use protocol::control::PROTOCOL_VERSION;

use super::HostEvent;
use super::accept::accept_viewers;
use super::devices::Devices;
use super::events::EventSink;
use super::session::SessionContext;
use super::state::SharedState;
use crate::handshake::generate_pin;
use crate::options::HostOptions;
use crate::paths;
use crate::trust::AuthorizedDevices;

/// Networking is light (one viewer, a control stream); the heavy work runs
/// on the streaming thread, so a small runtime is enough.
const RUNTIME_WORKERS: usize = 2;

pub(super) struct Service {
    state: Arc<SharedState>,
    devices: Arc<Devices>,
    events: EventSink,
    thread: Option<JoinHandle<()>>,
}

impl Service {
    /// Spawns the service thread and waits until it is listening (or
    /// failed to).
    pub(super) fn start(options: HostOptions, events: EventSink) -> Result<Self> {
        let pin = options.pin.clone().unwrap_or_else(generate_pin);
        let state = Arc::new(SharedState::new(pin, !options.view_only));
        let (store, load_warning) = AuthorizedDevices::load(paths::authorized_viewers_file()?);
        let devices = Arc::new(Devices::new(store, events.clone()));
        let (setup_tx, setup_rx) = channel();
        let thread = {
            let shared = Shared {
                state: state.clone(),
                devices: devices.clone(),
                events: events.clone(),
                load_warning,
            };
            thread::Builder::new()
                .name("wshare-host".into())
                .spawn(move || service_thread(&options, &shared, &setup_tx))
                .context("spawning the sharing service thread")?
        };
        match setup_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                state,
                devices,
                events,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(anyhow!("the sharing service exited during setup"))
            }
        }
    }

    pub(super) fn state(&self) -> &SharedState {
        &self.state
    }

    pub(super) fn devices(&self) -> &Devices {
        &self.devices
    }

    /// Rotates the PIN for future connections and reports it.
    pub(super) fn new_pin(&self) -> String {
        let pin = self.state.replace_pin(generate_pin());
        self.events.emit(HostEvent::PinChanged { pin: pin.clone() });
        pin
    }

    /// Stops and waits for the service thread (it emits `Stopped`).
    pub(super) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.state.request_stop();
        // Dropped from inside an observer callback, i.e. on the service's
        // own thread: waiting for ourselves would deadlock. The stop request
        // is enough; the thread finishes on its own.
        if thread.thread().id() != thread::current().id() {
            let _ = thread.join();
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// How `serve` ended.
enum Exit {
    /// Reported to `start` through the setup channel; no `Stopped` event.
    SetupFailed,
    /// `Ok`: stopped on request. `Err`: the endpoint failed on its own.
    Ended(Result<()>),
}

/// What the service thread shares with the handle.
struct Shared {
    state: Arc<SharedState>,
    devices: Arc<Devices>,
    events: EventSink,
    /// Why the authorized devices could not be loaded, reported once
    /// listening (the first event must be `Listening`).
    load_warning: Option<String>,
}

/// Body of the service thread. A panic is caught so the front-end always
/// learns that sharing ended (`Stopped { error: Some(..) }`), instead of
/// the UI showing "sharing" forever.
fn service_thread(options: &HostOptions, shared: &Shared, setup: &Sender<Result<()>>) {
    let events = &shared.events;
    let mut listening = false;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        serve(options, shared, setup, &mut listening)
    }));
    let error = match outcome {
        Ok(Exit::SetupFailed) => return,
        Ok(Exit::Ended(result)) => result.err().map(|e| format!("{e:#}")),
        Err(panic) if listening => Some(format!(
            "the sharing service crashed: {}",
            panic_message(&*panic)
        )),
        Err(panic) => {
            let message = format!(
                "the sharing service crashed during setup: {}",
                panic_message(&*panic)
            );
            let _ = setup.send(Err(anyhow!(message)));
            return;
        }
    };
    events.emit(HostEvent::Stopped { error });
}

fn serve(
    options: &HostOptions,
    shared: &Shared,
    setup: &Sender<Result<()>>,
    listening: &mut bool,
) -> Exit {
    let Shared {
        state,
        devices,
        events,
        load_warning,
    } = shared;
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(RUNTIME_WORKERS)
        .thread_name("wshare-host-io")
        .enable_all()
        .build()
        .context("starting the async runtime")
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let _ = setup.send(Err(e));
            return Exit::SetupFailed;
        }
    };
    let exit = runtime.block_on(async {
        let host = match Host::open(options, state.pin()) {
            Ok(host) => host,
            Err(e) => {
                let _ = setup.send(Err(e));
                return Exit::SetupFailed;
            }
        };
        events.emit(host.listening.clone());
        *listening = true;
        let _ = setup.send(Ok(()));
        if let Some(e) = &host.announcement_error {
            events.emit(HostEvent::Warning(format!(
                "LAN announcement failed: {e:#}"
            )));
        }
        devices.announce();
        if let Some(warning) = load_warning {
            events.emit(HostEvent::Warning(format!(
                "every viewer needs the PIN again: {warning}"
            )));
        }

        let context = SessionContext {
            host_name: host.name.clone(),
            stream: options.stream.clone(),
            state: Arc::clone(state),
            devices: Arc::clone(devices),
            events: events.clone(),
        };
        let result = accept_viewers(&host.endpoint, &context).await;
        host.close();
        Exit::Ended(result)
    });
    // Quinn's endpoint driver is a task of this runtime: the UDP port is
    // only released once the runtime is gone, so drop it before `Stopped`.
    drop(runtime);
    exit
}

/// The listening side, set up and announced.
struct Host {
    name: String,
    endpoint: HostEndpoint,
    advertiser: Option<Advertiser>,
    /// Discovery is a convenience (viewers can connect by IP), so a failed
    /// announcement is only a warning.
    announcement_error: Option<anyhow::Error>,
    listening: HostEvent,
}

impl Host {
    /// Must run inside the runtime (the endpoint registers with it).
    fn open(options: &HostOptions, pin: String) -> Result<Self> {
        // Cheapest check first: no point binding a port for a missing monitor.
        let monitor = find_monitor(options.stream.monitor)?;
        let name = computer_name();
        let identity = Identity::load_or_create(&paths::config_dir()?, &name)?;
        let endpoint = HostEndpoint::bind_any(options.port, &identity)
            .with_context(|| format!("listening on UDP port {}", options.port))?;
        let port = endpoint.local_addr()?.port();
        let fingerprint = identity.fingerprint();
        let announcement = HostAnnouncement {
            id: fingerprint.host_id(),
            display_name: name.clone(),
            port,
            protocol_version: PROTOCOL_VERSION,
            width: monitor.width,
            height: monitor.height,
        };
        let (advertiser, announcement_error) = match Advertiser::start(&announcement) {
            Ok(advertiser) => (Some(advertiser), None),
            Err(e) => (None, Some(e)),
        };
        let listening = HostEvent::Listening {
            computer_name: name.clone(),
            port,
            pin,
            fingerprint: fingerprint.to_string(),
            monitor: monitor.name,
            width: monitor.width,
            height: monitor.height,
        };
        Ok(Self {
            name,
            endpoint,
            advertiser,
            announcement_error,
            listening,
        })
    }

    /// Stops accepting, closes remaining connections and withdraws the LAN
    /// announcement (goodbye packet), so viewers stop listing this host.
    fn close(self) {
        self.endpoint.close();
        drop(self.advertiser);
    }
}

fn find_monitor(index: usize) -> Result<MonitorInfo> {
    capture::list_monitors()?
        .into_iter()
        .nth(index)
        .with_context(|| format!("monitor {index} not found"))
}

fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}
