//! The agent as seen from the service: `wshare service agent` started as
//! SYSTEM in a given session, on its `winsta0\default` desktop.
//!
//! - Its token is a copy of the service's (LocalSystem) moved to that
//!   session, so it may open the secure desktop.
//! - It is in a job object that kills it when the service lets go of it
//!   (or dies): no agent outlives the service.
//! - Its standard input is a pipe the service closes to ask it to stop; its
//!   standard output carries its requests (see [`AgentRequest`]); its
//!   standard error goes to the service log.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, WAIT_TIMEOUT,
};
use windows::Win32::Security::{
    DuplicateTokenEx, SECURITY_ATTRIBUTES, SecurityImpersonation, SetTokenInformation,
    TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_QUERY, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess,
    WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use super::command::AgentRequest;
use super::supervisor::AgentStatus;

/// The interactive desktop of the session; the agent's capture and input
/// threads move to the secure desktop by themselves when it shows.
const DESKTOP: &str = r"winsta0\default";
/// How long a stopping agent gets to say goodbye to its viewer.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Exit code of an agent that had to be ended.
const TERMINATED: u32 = 1;
/// What `ResumeThread` returns when it fails.
const RESUME_FAILED: u32 = u32::MAX;

pub(super) struct AgentProcess {
    process: OwnedHandle,
    /// Closing it ends the agent, whatever state it is in.
    _job: OwnedHandle,
    /// Closing it tells the agent to stop.
    stdin: Option<File>,
    reader: Option<JoinHandle<()>>,
    session: u32,
    started: Instant,
}

impl AgentProcess {
    /// Starts `exe service agent` in `session`; `on_request` gets each
    /// request it makes, on a thread of its own.
    pub(super) fn launch(
        exe: &Path,
        session: u32,
        stderr: File,
        on_request: impl Fn(AgentRequest) + Send + 'static,
    ) -> Result<Self> {
        let token = system_token_in(session)?;
        let (child_stdin, parent_stdin) = pipe()?;
        let (parent_stdout, child_stdout) = pipe()?;
        let child_stderr = OwnedHandle::from(stderr);
        for handle in [&child_stdin, &child_stdout, &child_stderr] {
            inheritable(handle, true)?;
        }
        for handle in [&parent_stdin, &parent_stdout] {
            inheritable(handle, false)?;
        }
        let process = create(exe, &token, [&child_stdin, &child_stdout, &child_stderr])?;
        // The agent holds its own copies now.
        drop((child_stdin, child_stdout, child_stderr));
        let job = match contain(&process).and_then(|job| resume(&process).map(|()| job)) {
            Ok(job) => job,
            Err(e) => {
                // SAFETY: our handle to the suspended process.
                let _ = unsafe { TerminateProcess(handle(&process.process), TERMINATED) };
                return Err(e);
            }
        };
        let reader = thread::Builder::new()
            .name("wshare-agent-requests".into())
            .spawn(move || read_requests(File::from(parent_stdout), on_request))
            .context("listening to the agent")?;
        Ok(Self {
            process: process.process,
            _job: job,
            stdin: Some(File::from(parent_stdin)),
            reader: Some(reader),
            session,
            started: Instant::now(),
        })
    }

    pub(super) fn status(&self) -> AgentStatus {
        // SAFETY: our process handle; a zero wait only polls.
        let waited = unsafe { WaitForSingleObject(handle(&self.process), 0) };
        AgentStatus {
            session: self.session,
            started: self.started,
            running: waited == WAIT_TIMEOUT,
        }
    }

    /// Asks the agent to stop (its viewer is told goodbye, keys released)
    /// and ends it if it does not in time.
    pub(super) fn stop(mut self) {
        drop(self.stdin.take());
        let grace = u32::try_from(STOP_GRACE.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: our process handle.
        if unsafe { WaitForSingleObject(handle(&self.process), grace) } == WAIT_TIMEOUT {
            // SAFETY: as above.
            let _ = unsafe { TerminateProcess(handle(&self.process), TERMINATED) };
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Passes on each request the agent writes, until it closes its output.
fn read_requests(output: File, on_request: impl Fn(AgentRequest)) {
    for line in BufReader::new(output).lines() {
        let Ok(line) = line else { break };
        if let Some(request) = AgentRequest::parse(&line) {
            on_request(request);
        }
    }
}

fn handle(owned: &OwnedHandle) -> HANDLE {
    HANDLE(owned.as_raw_handle())
}

/// # Safety
/// `raw` must be a valid handle that nothing else closes.
unsafe fn owned(raw: HANDLE) -> OwnedHandle {
    // SAFETY: per the contract.
    unsafe { OwnedHandle::from_raw_handle(raw.0) }
}

/// A primary token like the service's (LocalSystem), in `session`.
fn system_token_in(session: u32) -> Result<OwnedHandle> {
    let mut own = HANDLE::default();
    // SAFETY: valid out-parameter; the handle is owned right after.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE, &mut own) }
        .context("reading the service token")?;
    // SAFETY: just opened, closed by `own` only.
    let own = unsafe { owned(own) };
    let access = TOKEN_ASSIGN_PRIMARY
        | TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID;
    let mut copy = HANDLE::default();
    // SAFETY: valid handles and out-parameter.
    unsafe {
        DuplicateTokenEx(
            handle(&own),
            access,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut copy,
        )
    }
    .context("copying the service token")?;
    // SAFETY: just created, closed by the result only.
    let copy = unsafe { owned(copy) };
    // SAFETY: the session id is a u32, as TokenSessionId wants. Needs the
    // TCB privilege, which LocalSystem holds.
    unsafe {
        SetTokenInformation(
            handle(&copy),
            TokenSessionId,
            std::ptr::from_ref(&session).cast(),
            size_of::<u32>() as u32,
        )
    }
    .with_context(|| format!("moving the token to session {session}"))?;
    Ok(copy)
}

/// An anonymous pipe: (read end, write end).
fn pipe() -> Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: valid out-parameters; the handles are owned right after.
    unsafe { CreatePipe(&mut read, &mut write, None, 0) }.context("creating a pipe")?;
    // SAFETY: just created, each closed by its owner only.
    Ok(unsafe { (owned(read), owned(write)) })
}

/// Lets the agent inherit `handle`, or not.
fn inheritable(owned: &OwnedHandle, inherit: bool) -> Result<()> {
    let flags = if inherit {
        HANDLE_FLAG_INHERIT
    } else {
        HANDLE_FLAGS(0)
    };
    // SAFETY: our handle.
    unsafe { SetHandleInformation(handle(owned), HANDLE_FLAG_INHERIT.0, flags) }
        .context("preparing the agent's handles")
}

/// A process created suspended.
struct Created {
    process: OwnedHandle,
    thread: OwnedHandle,
}

/// Creates the agent, suspended, with `stdio` (input, output, error).
fn create(exe: &Path, token: &OwnedHandle, stdio: [&OwnedHandle; 3]) -> Result<Created> {
    let mut command: Vec<u16> = format!("\"{}\" service agent", exe.display())
        .encode_utf16()
        .chain([0])
        .collect();
    let mut desktop: Vec<u16> = DESKTOP.encode_utf16().chain([0]).collect();
    let [stdin, stdout, stderr] = stdio;
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: handle(stdin),
        hStdOutput: handle(stdout),
        hStdError: handle(stderr),
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every buffer outlives the call; only the handles marked
    // inheritable (the three of `stdio`) are passed on.
    unsafe {
        CreateProcessAsUserW(
            Some(handle(token)),
            &HSTRING::from(exe.as_os_str()),
            Some(PWSTR(command.as_mut_ptr())),
            None::<*const SECURITY_ATTRIBUTES>,
            None::<*const SECURITY_ATTRIBUTES>,
            true,
            CREATE_SUSPENDED | CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    }
    .context("starting the agent")?;
    // SAFETY: just created, each closed by its owner only.
    Ok(unsafe {
        Created {
            process: owned(info.hProcess),
            thread: owned(info.hThread),
        }
    })
}

/// Lets the suspended process run.
fn resume(process: &Created) -> Result<()> {
    // SAFETY: the main thread of the suspended process, ours to resume.
    if unsafe { ResumeThread(handle(&process.thread)) } == RESUME_FAILED {
        return Err(std::io::Error::last_os_error()).context("resuming the agent");
    }
    Ok(())
}

/// Puts the process in a job that kills it once the job handle is closed.
fn contain(process: &Created) -> Result<OwnedHandle> {
    // SAFETY: no name, default security; the handle is owned right after.
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.context("creating a job")?;
    // SAFETY: just created, closed by the result only.
    let job = unsafe { owned(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the structure and its size match the information class.
    unsafe {
        SetInformationJobObject(
            handle(&job),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }
    .context("setting up the job")?;
    // SAFETY: our job and process handles.
    unsafe { AssignProcessToJobObject(handle(&job), handle(&process.process)) }
        .context("putting the agent in its job")?;
    Ok(job)
}
