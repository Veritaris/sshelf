//! Windows: state of the built-in `ssh-agent` service (SPEC.md §8). Only
//! queried; enabling it needs admin rights and is left to the user.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartType {
    Automatic,
    Manual,
    Disabled,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentService {
    NotInstalled,
    Installed {
        running: bool,
        start_type: StartType,
    },
}

/// Shown when the service is not running.
pub const ENABLE_HINT: &str = "Run in an administrator PowerShell:\n    \
    Get-Service ssh-agent | Set-Service -StartupType Automatic\n    \
    Start-Service ssh-agent";

impl AgentService {
    pub fn is_running(&self) -> bool {
        matches!(self, AgentService::Installed { running: true, .. })
    }
}

/// The service state; `None` outside Windows.
pub fn query() -> Option<Result<AgentService, String>> {
    #[cfg(windows)]
    {
        Some(windows::query())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod windows {
    use std::{mem, ptr};

    use windows_sys::Win32::Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_DOES_NOT_EXIST, GetLastError,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
        QueryServiceConfigW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_AUTO_START,
        SERVICE_DEMAND_START, SERVICE_DISABLED, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS,
        SERVICE_RUNNING, SERVICE_STATUS,
    };

    use super::{AgentService, StartType};

    /// Closes a service handle on drop.
    struct Handle(SC_HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful Open* call and is closed once.
            unsafe { CloseServiceHandle(self.0) };
        }
    }

    pub fn query() -> Result<AgentService, String> {
        let name: Vec<u16> = "ssh-agent".encode_utf16().chain([0]).collect();
        // SAFETY: plain Win32 calls with NUL-terminated strings, valid out
        // pointers and a buffer of the size the API asked for.
        unsafe {
            let scm = OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT);
            if scm.is_null() {
                return Err(format!(
                    "cannot open the service manager (error {})",
                    GetLastError()
                ));
            }
            let scm = Handle(scm);
            let service = OpenServiceW(
                scm.0,
                name.as_ptr(),
                SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG,
            );
            if service.is_null() {
                let err = GetLastError();
                return if err == ERROR_SERVICE_DOES_NOT_EXIST {
                    Ok(AgentService::NotInstalled)
                } else {
                    Err(format!("cannot open the ssh-agent service (error {err})"))
                };
            }
            let service = Handle(service);

            let mut status: SERVICE_STATUS = mem::zeroed();
            if QueryServiceStatus(service.0, &mut status) == 0 {
                return Err(format!("cannot query ssh-agent (error {})", GetLastError()));
            }

            let mut needed = 0u32;
            QueryServiceConfigW(service.0, ptr::null_mut(), 0, &mut needed);
            if GetLastError() != ERROR_INSUFFICIENT_BUFFER {
                return Err(format!(
                    "cannot query the ssh-agent configuration (error {})",
                    GetLastError()
                ));
            }
            // u64 storage keeps the struct suitably aligned.
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            let config = buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
            if QueryServiceConfigW(service.0, config, (buffer.len() * 8) as u32, &mut needed) == 0 {
                return Err(format!(
                    "cannot query the ssh-agent configuration (error {})",
                    GetLastError()
                ));
            }
            let start_type = match (*config).dwStartType {
                SERVICE_AUTO_START => StartType::Automatic,
                SERVICE_DEMAND_START => StartType::Manual,
                SERVICE_DISABLED => StartType::Disabled,
                _ => StartType::Other,
            };
            Ok(AgentService::Installed {
                running: status.dwCurrentState == SERVICE_RUNNING,
                start_type,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_available_only_on_windows() {
        let result = query();
        if cfg!(windows) {
            // CI runners and dev machines have the service installed (often disabled).
            let state = result.expect("queried on Windows");
            assert!(state.is_ok(), "{state:?}");
        } else {
            assert!(result.is_none());
        }
    }

    #[test]
    fn running_check() {
        let running = AgentService::Installed {
            running: true,
            start_type: StartType::Automatic,
        };
        assert!(running.is_running());
        assert!(!AgentService::NotInstalled.is_running());
    }
}
