//! The Service Control Manager, for the unattended-access service: install
//! or update it, start, stop and delete it, read its state. Thin wrappers
//! over the Win32 calls, with handles closed on drop.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST,
    ERROR_SERVICE_NOT_ACTIVE,
};
use windows::Win32::Storage::FileSystem::DELETE;
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_ACTION,
    SC_ACTION_RESTART, SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE,
    SERVICE_AUTO_START, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW,
    SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONSW, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_START, SERVICE_STATUS, SERVICE_STOP, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS,
    StartServiceW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use super::SERVICE_NAME;
use super::elevation::ServiceState;

const DISPLAY_NAME: &str = "wshare - accesso non presidiato";
const DESCRIPTION: &str = "Permette di usare questo PC dalla rete locale anche senza nessuno \
     davanti (schermata di blocco, UAC, Ctrl+Alt+Canc). Installato da wshare.";
/// The `DELETE` standard access right, needed to remove the service.
const DELETE_ACCESS: u32 = DELETE.0;
/// After a crash Windows starts the service again after this delay.
const RESTART_DELAY: Duration = Duration::from_secs(5);
/// A day without crashes resets the count of restarts.
const FAILURE_RESET: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the state is read while waiting for the service to stop.
const POLL: Duration = Duration::from_millis(200);

/// An open SCM or service handle.
struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed once.
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn manager(access: u32) -> Result<Handle> {
    // SAFETY: plain Win32 call; the handle is owned by the result.
    unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access) }
        .map(Handle)
        .map_err(|e| denied_or(e, "opening the service manager"))
}

/// The service, if it is installed.
fn open(manager: &Handle, access: u32) -> Result<Option<Handle>> {
    // SAFETY: plain Win32 call; the handle is owned by the result.
    match unsafe { OpenServiceW(manager.0, &HSTRING::from(SERVICE_NAME), access) } {
        Ok(handle) => Ok(Some(Handle(handle))),
        Err(e) if e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => Ok(None),
        Err(e) => Err(denied_or(e, "opening the wshare service")),
    }
}

/// An access denied means the command was not run as an administrator.
fn denied_or(error: windows::core::Error, doing: &str) -> anyhow::Error {
    if error.code() == ERROR_ACCESS_DENIED.to_hresult() {
        anyhow::anyhow!("{doing}: access denied (run it as an administrator)")
    } else {
        anyhow::Error::from(error).context(doing.to_string())
    }
}

/// Fails unless this process may install services: an administrator, with
/// rights raised (UAC).
pub(super) fn ensure_administrator() -> Result<()> {
    manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE).map(drop)
}

/// Whether the service is installed, and running. Needs no administrator.
pub(super) fn state() -> Result<ServiceState> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let Some(service) = open(&manager, SERVICE_QUERY_STATUS)? else {
        return Ok(ServiceState::NotInstalled);
    };
    let status = query(&service)?;
    Ok(if status.dwCurrentState == SERVICE_RUNNING {
        ServiceState::Running
    } else {
        ServiceState::Stopped
    })
}

fn query(service: &Handle) -> Result<SERVICE_STATUS> {
    let mut status = SERVICE_STATUS::default();
    // SAFETY: `status` is a valid out-parameter.
    unsafe { QueryServiceStatus(service.0, &mut status) }.context("reading the service state")?;
    Ok(status)
}

/// Creates the service running `exe service run` as LocalSystem and
/// started with Windows, or points the existing one at `exe`.
pub(super) fn install(exe: &Path) -> Result<()> {
    let manager = manager(SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE)?;
    let command = HSTRING::from(format!("\"{}\" service run", exe.display()));
    let display = HSTRING::from(DISPLAY_NAME);
    let access = SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS | SERVICE_CHANGE_CONFIG;
    let service = match open(&manager, access)? {
        // SAFETY: plain Win32 call on our handle; the strings outlive it.
        Some(service) => unsafe {
            ChangeServiceConfigW(
                service.0,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                &command,
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
                &display,
            )
            .context("updating the wshare service")?;
            service
        },
        // SAFETY: as above; LocalSystem is the default account (null name).
        None => unsafe {
            CreateServiceW(
                manager.0,
                &HSTRING::from(SERVICE_NAME),
                &display,
                access,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                &command,
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
            )
            .map(Handle)
            .map_err(|e| denied_or(e, "creating the wshare service"))?
        },
    };
    describe(&service)?;
    restart_on_failure(&service)
}

fn describe(service: &Handle) -> Result<()> {
    let mut text: Vec<u16> = DESCRIPTION.encode_utf16().chain([0]).collect();
    let description = SERVICE_DESCRIPTIONW {
        lpDescription: PWSTR(text.as_mut_ptr()),
    };
    // SAFETY: `description` and its text outlive the call.
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some(std::ptr::from_ref(&description).cast()),
        )
    }
    .context("describing the wshare service")
}

/// A crashed service is started again, so unattended access does not stay
/// off until the next restart of Windows.
fn restart_on_failure(service: &Handle) -> Result<()> {
    let delay = u32::try_from(RESTART_DELAY.as_millis()).unwrap_or(u32::MAX);
    let mut actions = [SC_ACTION {
        Type: SC_ACTION_RESTART,
        Delay: delay,
    }; 3];
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: u32::try_from(FAILURE_RESET.as_secs()).unwrap_or(u32::MAX),
        lpRebootMsg: PWSTR::null(),
        lpCommand: PWSTR::null(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    // SAFETY: `failure` and the actions it points to outlive the call.
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            Some(std::ptr::from_ref(&failure).cast()),
        )
    }
    .context("setting the restart of the wshare service")
}

/// Starts the service (already running is fine).
pub(super) fn start() -> Result<()> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let service = open(&manager, SERVICE_START)?.context("the wshare service is not installed")?;
    // SAFETY: plain Win32 call on our handle, no arguments.
    match unsafe { StartServiceW(service.0, None) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => Ok(()),
        Err(e) => Err(denied_or(e, "starting the wshare service")),
    }
}

/// Stops the service, if installed and running, and waits until it has
/// stopped (at most `timeout`).
pub(super) fn stop(timeout: Duration) -> Result<()> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let Some(service) = open(&manager, SERVICE_STOP | SERVICE_QUERY_STATUS)? else {
        return Ok(());
    };
    stop_service(&service, timeout)
}

fn stop_service(service: &Handle, timeout: Duration) -> Result<()> {
    let mut status = SERVICE_STATUS::default();
    // SAFETY: `status` is a valid out-parameter.
    match unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => return Ok(()),
        Err(e) => return Err(denied_or(e, "stopping the wshare service")),
    }
    let deadline = Instant::now() + timeout;
    while query(service)?.dwCurrentState != SERVICE_STOPPED {
        if Instant::now() >= deadline {
            bail!("the wshare service did not stop in time");
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// Stops (waiting at most `timeout`) and deletes the service. `false`: it
/// was not installed.
pub(super) fn remove(timeout: Duration) -> Result<bool> {
    let manager = manager(SC_MANAGER_CONNECT)?;
    let access = SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE_ACCESS;
    let Some(service) = open(&manager, access)? else {
        return Ok(false);
    };
    stop_service(&service, timeout)?;
    // SAFETY: plain Win32 call on our handle. Windows removes the service
    // (and its registry key) once the last handle to it is closed.
    unsafe { DeleteService(service.0) }.map_err(|e| denied_or(e, "removing the wshare service"))?;
    Ok(true)
}
