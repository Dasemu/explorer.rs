// SPDX-License-Identifier: GPL-3.0-or-later
//! Job object so supervised children die with the supervisor.
//!
//! Without this, closing the supervisor's console in dev mode would leave
//! orphaned taskbars reserving screen space.

use std::os::windows::io::AsRawHandle;

use anyhow::{Context, Result};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

pub struct KillOnCloseJob(HANDLE);

// SAFETY: a job handle is a kernel handle usable from any thread.
unsafe impl Send for KillOnCloseJob {}

impl KillOnCloseJob {
    pub fn new() -> Result<Self> {
        // SAFETY: anonymous job; info struct correctly sized.
        unsafe {
            let job = CreateJobObjectW(None, None).context("CreateJobObjectW")?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .context("SetInformationJobObject")?;
            Ok(Self(job))
        }
    }

    pub fn assign(&self, child: &std::process::Child) -> Result<()> {
        let h = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { AssignProcessToJobObject(self.0, h) }.context("AssignProcessToJobObject")
    }
}
// The job handle is intentionally never closed explicitly: it is released
// when the supervisor process exits, which is exactly when children must die.
