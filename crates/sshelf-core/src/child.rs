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
    /// The tree a process `pid` that leads its own group started, e.g. one
    /// sshelf ran before it crashed (SPEC.md §7 *Surviving a crash*).
    #[cfg(unix)]
    pub fn adopt(pid: u32) -> Self {
        Self { pgid: pid as i32 }
    }

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

/// When process `pid` started, as a number that changes when the pid is
/// reused: `None` if there is no such process, or this OS isn't covered
/// (Windows needs none: its tunnels die with sshelf).
pub fn start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // Field 22 of /proc/<pid>/stat, in clock ticks since boot; the
        // fields are counted after the command name, which may hold spaces.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = &stat[stat.rfind(')')? + 1..];
        after_name.split_whitespace().nth(19)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        // SAFETY: `info` is a writable `proc_bsdinfo` of `size` bytes.
        let read = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        (read == size).then(|| info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Blocks until process `pid`, which need not be a child of this one, has
/// exited (or returns at once if it is gone).
#[cfg(unix)]
pub fn wait_exit(pid: u32) {
    unix::wait_exit(pid);
}

#[cfg(unix)]
mod unix {
    use std::thread;
    use std::time::Duration;

    use super::KILL_GRACE;

    /// How often a process without an exit notification is looked at.
    const POLL: Duration = Duration::from_secs(1);

    fn interrupted() -> bool {
        std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
    }

    fn process_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks whether the process exists.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Linux: a pidfd turns readable when the process exits. Kernels
    /// before 5.3 have none; then it is looked at every second.
    #[cfg(target_os = "linux")]
    pub fn wait_exit(pid: u32) {
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) } as i32;
        if fd >= 0 {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd; the fd is closed once, below.
            unsafe {
                while libc::poll(&mut poll, 1, -1) < 0 && interrupted() {}
                libc::close(fd);
            }
            return;
        }
        while process_alive(pid as i32) {
            thread::sleep(POLL);
        }
    }

    /// macOS: kqueue reports the process's exit.
    #[cfg(target_os = "macos")]
    pub fn wait_exit(pid: u32) {
        // SAFETY: a new kqueue; closed below.
        let kq = unsafe { libc::kqueue() };
        if kq >= 0 {
            let change = libc::kevent {
                ident: pid as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: one change in, one event out, no timeout.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let registered =
                unsafe { libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
            if registered == 0 {
                unsafe {
                    while libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) < 0
                        && interrupted()
                    {}
                    libc::close(kq);
                }
                return;
            }
            // SAFETY: closed once.
            unsafe { libc::close(kq) };
        }
        while process_alive(pid as i32) {
            thread::sleep(POLL);
        }
    }

    /// Elsewhere: looked at every second.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn wait_exit(pid: u32) {
        while process_alive(pid as i32) {
            thread::sleep(POLL);
        }
    }

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
