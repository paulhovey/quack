//! Windows Job Objects: every process a command starts is killed with it.
#![allow(unsafe_code)]

use std::os::windows::io::RawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

/// An anonymous job with kill-on-close: dropping it (or the harness exiting, however it
/// exits) ends every process in it.
#[derive(Debug)]
pub(crate) struct Job(HANDLE);

// SAFETY: a job object handle may be used and closed from any thread.
unsafe impl Send for Job {}
// SAFETY: the job APIs used here are thread-safe on a shared handle.
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> Option<Self> {
        // SAFETY: null attributes and name create an anonymous job; null means failure.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return None;
        }
        let job = Self(handle);
        // SAFETY: the struct is plain data; all-zero is a valid "no limits" value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).ok()?;
        // SAFETY: `info` lives across the call and `size` matches its type.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size,
            )
        };
        (ok != 0).then_some(job)
    }

    /// Put a running process (and so its future children) in the job.
    pub fn assign(&self, process: RawHandle) -> bool {
        // SAFETY: `process` is a live process handle owned by the caller's `Child`.
        unsafe { AssignProcessToJobObject(self.0, process.cast()) != 0 }
    }

    pub fn kill(&self) {
        // SAFETY: the handle is valid until drop.
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}
