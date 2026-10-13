//! Keep a tool and its descendants inside an owned Windows job before it runs.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

pub(crate) struct Job(OwnedHandle);

impl Job {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: null attributes create an unnamed, non-inheritable job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a new owned handle.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        // SAFETY: zero is valid for unused fields in this limits structure.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the structure is initialized and lives through the call.
        let configured = unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(crate) fn spawn(&self, command: &mut Command) -> io::Result<Child> {
        // Command's stable Windows API closes the primary thread handle. Start
        // suspended, then recover its thread through ToolHelp, so no launcher
        // can create an unsupervised descendant between spawn and assignment.
        command.creation_flags(super::CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let prepare = || {
            // SAFETY: both handles remain owned and alive throughout the call.
            if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            resume_primary_thread(child.id())
        };
        if let Err(error) = prepare() {
            // Before resume the tool cannot spawn children. Failed supervision
            // is an error; never fall back to running without the job.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(child)
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        // SAFETY: the job handle is owned for the lifetime of the tool run.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn resume_primary_thread(pid: u32) -> io::Result<()> {
    // SAFETY: snapshot creation has no borrowed pointers.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the snapshot is a new valid owned handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    // SAFETY: THREADENTRY32 is initialized before ToolHelp writes to it.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    // SAFETY: snapshot and entry remain valid throughout enumeration.
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut thread = None;
    loop {
        if entry.th32OwnerProcessID == pid && thread.replace(entry.th32ThreadID).is_some() {
            return Err(io::Error::other("suspended tool has more than one thread"));
        }
        // SAFETY: same valid snapshot and initialized entry as above.
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            break;
        }
    }
    let id = thread.ok_or_else(|| io::Error::other("suspended tool has no primary thread"))?;
    // SAFETY: only the newly spawned suspended tool's thread is opened.
    let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, id) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenThread returned a new owned thread handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    // SAFETY: the handle has THREAD_SUSPEND_RESUME and remains alive.
    match unsafe { ResumeThread(handle.as_raw_handle()) } {
        1 => Ok(()),
        u32::MAX => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other(
            "tool primary thread did not resume from CREATE_SUSPENDED",
        )),
    }
}
