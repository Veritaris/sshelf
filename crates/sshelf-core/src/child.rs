//! Long-running child processes (tunnels) that are stopped together with
//! everything they started, e.g. a `ProxyCommand` (SPEC.md §7).
//!
//! - Windows: the process runs in a Job Object with "kill on close"; stopping
//!   terminates the job, and if sshelf itself dies the OS closes the handle
//!   and the whole tree goes with it. No console window is created.
//! - Unix: the process leads its own process group; stopping sends SIGTERM to
//!   the group and SIGKILL after a grace period.
//!
//! The [`Child`] is handed to a waiter (it blocks on `wait`), while
//! [`TreeKiller`] stops the tree from anywhere without needing the `Child`.

use std::io;
use std::process::{Child, Command};
use std::time::Duration;

/// Time between SIGTERM and SIGKILL on Unix.
pub const KILL_GRACE: Duration = Duration::from_secs(2);

pub struct Spawned {
    pub child: Child,
    pub killer: TreeKiller,
}

/// Starts `cmd` in its own process tree.
pub fn spawn_tree(mut cmd: Command) -> io::Result<Spawned> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
        // The job exists before the process so it cannot outlive a failed setup.
        let job = windows::Job::new()?;
        let child = cmd.spawn()?;
        if let Err(e) = job.assign(&child) {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok(Spawned {
            child,
            killer: TreeKiller { job },
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let pgid = child.id() as i32;
        Ok(Spawned {
            child,
            killer: TreeKiller { pgid },
        })
    }
}

/// Stops a process tree. Dropping it stops the tree too.
pub struct TreeKiller {
    #[cfg(windows)]
    job: windows::Job,
    #[cfg(unix)]
    pgid: i32,
}

impl TreeKiller {
    /// Stops every process in the tree; returns at once (Unix escalates to
    /// SIGKILL in the background if needed).
    pub fn kill(&self) {
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        unix::terminate(self.pgid);
    }
}

impl Drop for TreeKiller {
    fn drop(&mut self) {
        // Windows: closing the job handle kills what is left.
        #[cfg(unix)]
        unix::terminate(self.pgid);
    }
}

#[cfg(unix)]
mod unix {
    use std::thread;

    use super::KILL_GRACE;

    fn alive(pgid: i32) -> bool {
        // SAFETY: signal 0 only checks whether the group exists.
        unsafe { libc::kill(-pgid, 0) == 0 }
    }

    pub fn terminate(pgid: i32) {
        if !alive(pgid) {
            return;
        }
        // SAFETY: plain signal delivery to the group this module created.
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        thread::spawn(move || {
            thread::sleep(KILL_GRACE);
            if alive(pgid) {
                // SAFETY: as above.
                unsafe { libc::kill(-pgid, libc::SIGKILL) };
            }
        });
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::{io, mem, ptr};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> io::Result<Self> {
            // SAFETY: plain Win32 calls with valid pointers; the handle is
            // owned by `Job` and closed in `Drop`.
            unsafe {
                let handle = CreateJobObjectW(ptr::null(), ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Job(handle);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        pub fn assign(&self, child: &Child) -> io::Result<()> {
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub fn terminate(&self) {
            // SAFETY: valid job handle; terminating an empty job is harmless.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is owned and closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
}
