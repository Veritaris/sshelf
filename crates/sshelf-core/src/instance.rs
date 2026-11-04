//! One sshelf per user: the first start holds an exclusive lock on
//! `instance.lock` in the app folder, and a later start asks it to show its
//! window instead of opening a second one (two supervisors would fight over
//! the same ports and files).
//!
//! The lock is an OS file lock, released by the OS on any exit (a crash
//! included), so a stale lock never blocks a start. The "show" request goes
//! over a local channel, never a network interface:
//! - Windows: a named event in the logon session's namespace
//!   (`Local\sshelf-show-<hash of the app folder>`), set by the later start;
//! - Linux / macOS: a Unix domain socket, `instance.sock` in the app folder.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

const LOCK_FILE: &str = "instance.lock";

/// The running instance: holds the lock and listens for "show" requests
/// until dropped.
pub struct Instance {
    _lock: File,
    stop: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    channel: Channel,
}

/// Takes the instance lock in `app_dir` (created if missing). `Ok(None)`:
/// another sshelf holds it; ask it with [`show_running`]. `on_show` runs on
/// a background thread each time a later start asks.
pub fn acquire(
    app_dir: &Path,
    on_show: impl Fn() + Send + 'static,
) -> io::Result<Option<Instance>> {
    fs::create_dir_all(app_dir)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(app_dir.join(LOCK_FILE))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => return Err(e),
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (channel, listener) = Channel::listen(app_dir, Arc::clone(&stop), on_show)?;
    Ok(Some(Instance {
        _lock: lock,
        stop,
        listener: Some(listener),
        channel,
    }))
}

/// Asks the sshelf holding the lock in `app_dir` to show its window.
pub fn show_running(app_dir: &Path) -> io::Result<()> {
    Channel::notify(app_dir)
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the listener so it sees `stop`.
        self.channel.wake();
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        self.channel.close();
    }
}

#[cfg(unix)]
struct Channel {
    path: PathBuf,
}

#[cfg(unix)]
impl Channel {
    fn socket(app_dir: &Path) -> PathBuf {
        app_dir.join("instance.sock")
    }

    fn listen(
        app_dir: &Path,
        stop: Arc<AtomicBool>,
        on_show: impl Fn() + Send + 'static,
    ) -> io::Result<(Self, JoinHandle<()>)> {
        use std::io::Read;
        use std::os::unix::net::UnixListener;

        let path = Self::socket(app_dir);
        // We hold the lock, so a socket file left there is from a run that
        // ended without cleaning up.
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let thread = thread::Builder::new()
            .name("sshelf-instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else {
                        continue;
                    };
                    let mut request = [0u8; 8];
                    let n = stream.read(&mut request).unwrap_or(0);
                    if request[..n].starts_with(b"show") {
                        on_show();
                    }
                }
            })?;
        Ok((Self { path }, thread))
    }

    fn notify(app_dir: &Path) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        UnixStream::connect(Self::socket(app_dir))?.write_all(b"show\n")
    }

    fn wake(&self) {
        let _ = std::os::unix::net::UnixStream::connect(&self.path);
    }

    fn close(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
struct Channel {
    /// The event's handle (a `HANDLE`, kept as an integer to be `Send`).
    event: usize,
}

#[cfg(windows)]
impl Channel {
    /// The event's name: per logon session, and per app folder so separate
    /// app folders (tests) never meet.
    fn name(app_dir: &Path) -> Vec<u16> {
        // FNV-1a: stable across builds, unlike std's hasher.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in app_dir.to_string_lossy().to_lowercase().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("Local\\sshelf-show-{hash:016x}")
            .encode_utf16()
            .chain([0])
            .collect()
    }

    fn listen(
        app_dir: &Path,
        stop: Arc<AtomicBool>,
        on_show: impl Fn() + Send + 'static,
    ) -> io::Result<(Self, JoinHandle<()>)> {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};

        let name = Self::name(app_dir);
        // SAFETY: a NUL-terminated name; an auto-reset event, not signalled.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = event as usize;
        let thread = thread::Builder::new()
            .name("sshelf-instance".into())
            .spawn(move || {
                loop {
                    // SAFETY: the handle stays open until this thread has
                    // ended (`Drop` joins it before `close`).
                    let woke = unsafe { WaitForSingleObject(handle as _, INFINITE) };
                    if woke != WAIT_OBJECT_0 || stop.load(Ordering::SeqCst) {
                        break;
                    }
                    on_show();
                }
            })?;
        Ok((Self { event: handle }, thread))
    }

    fn notify(app_dir: &Path) -> io::Result<()> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};

        let name = Self::name(app_dir);
        // SAFETY: a NUL-terminated name; the handle is closed below.
        unsafe {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            let set = SetEvent(event);
            CloseHandle(event);
            if set == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn wake(&self) {
        // SAFETY: our own open event handle.
        unsafe { windows_sys::Win32::System::Threading::SetEvent(self.event as _) };
    }

    fn close(&self) {
        // SAFETY: our own handle; the listener thread has ended.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.event as _) };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn counting() -> (impl Fn() + Send + 'static, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        (
            move || {
                let _ = tx.send(());
            },
            rx,
        )
    }

    #[test]
    fn the_first_start_holds_it_and_a_later_one_is_told() {
        let tmp = tempfile::tempdir().unwrap();
        let (on_show, shown) = counting();
        let first = acquire(tmp.path(), on_show).unwrap();
        assert!(first.is_some(), "the first start gets the lock");
        let (second_show, _) = counting();
        assert!(
            acquire(tmp.path(), second_show).unwrap().is_none(),
            "a later start finds it taken"
        );
        show_running(tmp.path()).unwrap();
        shown
            .recv_timeout(Duration::from_secs(5))
            .expect("the running instance was asked to show");
        show_running(tmp.path()).unwrap();
        shown
            .recv_timeout(Duration::from_secs(5))
            .expect("and again");
    }

    #[test]
    fn the_lock_is_free_again_once_the_instance_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let (on_show, _) = counting();
        let first = acquire(tmp.path(), on_show).unwrap();
        drop(first);
        let (on_show, _) = counting();
        assert!(acquire(tmp.path(), on_show).unwrap().is_some());
        // Nobody runs in another folder: nothing to ask there.
        let other = tempfile::tempdir().unwrap();
        assert!(show_running(other.path()).is_err());
    }

    #[test]
    fn separate_app_folders_do_not_meet() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (show_a, shown_a) = counting();
        let (show_b, shown_b) = counting();
        let _a = acquire(a.path(), show_a).unwrap().unwrap();
        let _b = acquire(b.path(), show_b).unwrap().unwrap();
        show_running(b.path()).unwrap();
        shown_b.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(shown_a.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
