//! `wshare service run`: the Windows service itself (LocalSystem).
//!
//! It hands its thread to the Service Control Manager, which calls back
//! [`service_main`]; that reports "running" and supervises the agent (see
//! `supervisor`) until Windows asks it to stop. Session changes (sign-in,
//! sign-out, user switch, lock) wake it at once; it also looks every
//! [`TICK`] in case the agent ended.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use windows::Win32::Foundation::{
    ERROR_CALL_NOT_IMPLEMENTED, ERROR_SERVICE_SPECIFIC_ERROR, NO_ERROR,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_STATUS,
    SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows::core::{HSTRING, PWSTR};

use super::SERVICE_NAME;
use super::agent_process::AgentProcess;
use super::command::AgentRequest;
use super::locations;
use super::secure_attention;
use super::service_log::ServiceLog;
use super::supervisor::{Decision, Supervisor};

/// How often the agent is looked at without a session change.
const TICK: Duration = Duration::from_secs(1);
/// What `WTSGetActiveConsoleSessionId` says while no session is attached
/// to the console (e.g. while switching).
const NO_SESSION: u32 = u32::MAX;
/// What Windows is told the stop may take: the agent gets a few seconds to
/// say goodbye to its viewer.
const STOP_WAIT_HINT: Duration = Duration::from_secs(10);

/// What the control handler tells the service thread.
enum Signal {
    Stop,
    SessionChanged,
}

/// The handler runs on a thread of the SCM's: it only passes signals on.
static SIGNALS: OnceLock<Mutex<Sender<Signal>>> = OnceLock::new();
/// Why the service failed, for `run` to return.
static FAILURE: OnceLock<String> = OnceLock::new();

/// Runs the service until Windows stops it. Fails at once when not
/// started by Windows.
pub(super) fn run() -> Result<()> {
    let mut name: Vec<u16> = SERVICE_NAME.encode_utf16().chain([0]).collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: the table (and the name it points to) outlives the call,
    // which returns once the service has stopped.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
        .context("this command is started by Windows (see `wshare service install`)")?;
    match FAILURE.get() {
        Some(failure) => Err(anyhow!("{failure}")),
        None => Ok(()),
    }
}

/// Called by the SCM on a thread of its own.
unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let outcome = catch_unwind(AssertUnwindSafe(serve));
    let failure = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(e)) => format!("{e:#}"),
        Err(_) => "the service panicked".to_string(),
    };
    let _ = FAILURE.set(failure);
}

fn serve() -> Result<()> {
    let (signals, received) = channel();
    SIGNALS
        .set(Mutex::new(signals))
        .map_err(|_| anyhow!("the service started twice"))?;
    // SAFETY: the name outlives the call; the handler is a plain function.
    let status =
        unsafe { RegisterServiceCtrlHandlerExW(&HSTRING::from(SERVICE_NAME), Some(handler), None) }
            .context("registering the service handler")?;
    report(status, SERVICE_RUNNING, NO_ERROR.0);
    let result = supervise(&received, || {
        report(status, SERVICE_STOP_PENDING, NO_ERROR.0)
    });
    let exit = if result.is_ok() {
        NO_ERROR.0
    } else {
        ERROR_SERVICE_SPECIFIC_ERROR.0
    };
    report(status, SERVICE_STOPPED, exit);
    result
}

/// Tells Windows the state of the service.
fn report(status: SERVICE_STATUS_HANDLE, state: SERVICE_STATUS_CURRENT_STATE, exit: u32) {
    let accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
    } else {
        0
    };
    let wait_hint = if state == SERVICE_STOP_PENDING {
        u32::try_from(STOP_WAIT_HINT.as_millis()).unwrap_or(u32::MAX)
    } else {
        0
    };
    let service_status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: exit,
        dwWaitHint: wait_hint,
        ..Default::default()
    };
    // SAFETY: the handle came from RegisterServiceCtrlHandlerExW.
    let _ = unsafe { SetServiceStatus(status, &service_status) };
}

/// Called by the SCM for every control code.
unsafe extern "system" fn handler(
    control: u32,
    _event: u32,
    _data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    let signal = match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => Signal::Stop,
        SERVICE_CONTROL_SESSIONCHANGE => Signal::SessionChanged,
        SERVICE_CONTROL_INTERROGATE => return NO_ERROR.0,
        _ => return ERROR_CALL_NOT_IMPLEMENTED.0,
    };
    if let Some(signals) = SIGNALS.get() {
        let _ = signals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(signal);
    }
    NO_ERROR.0
}

/// Keeps an agent in the console session until told to stop; `stopping`
/// tells Windows the stop has begun.
fn supervise(signals: &Receiver<Signal>, stopping: impl Fn()) -> Result<()> {
    let data = locations::data_dir()?;
    let log = Arc::new(ServiceLog::open(&locations::log_file(&data))?);
    log.write("service started");
    let exe = std::env::current_exe().context("locating wshare.exe")?;
    let mut supervisor = Supervisor::default();
    let mut agent: Option<AgentProcess> = None;
    loop {
        let now = Instant::now();
        let status = agent.as_ref().map(AgentProcess::status);
        match supervisor.decide(console_session(), status, now) {
            Decision::Keep => {}
            Decision::Launch(session) => match launch(&exe, session, &log) {
                Ok(started) => {
                    log.write(format_args!("agent started in session {session}"));
                    agent = Some(started);
                }
                Err(e) => {
                    log.write(format_args!("cannot start the agent: {e:#}"));
                    supervisor.launch_failed(session, now);
                }
            },
            Decision::Stop | Decision::Reap => {
                if let Some(ended) = agent.take() {
                    ended.stop();
                    log.write("agent stopped");
                }
                continue;
            }
        }
        match signals.recv_timeout(TICK) {
            Ok(Signal::SessionChanged) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    stopping();
    if let Some(running) = agent.take() {
        running.stop();
    }
    log.write("service stopped");
    Ok(())
}

fn launch(exe: &Path, session: u32, log: &Arc<ServiceLog>) -> Result<AgentProcess> {
    let requests_log = Arc::clone(log);
    AgentProcess::launch(exe, session, log.reopen()?, move |request| match request {
        AgentRequest::SecureAttention => {
            if let Err(e) = secure_attention::send() {
                requests_log.write(format_args!("Ctrl+Alt+Del failed: {e:#}"));
            }
        }
    })
}

/// The session attached to the console, if any.
fn console_session() -> Option<u32> {
    // SAFETY: no preconditions.
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    (session != NO_SESSION).then_some(session)
}
