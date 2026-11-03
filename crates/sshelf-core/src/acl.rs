//! Windows file ACLs for new private keys (SPEC.md §11): OpenSSH for Windows
//! refuses a key that other users can read, so the file's DACL is replaced
//! by a single full-access entry for the current user, with inheritance cut.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, GENERIC_ALL, HANDLE, LocalFree, WIN32_ERROR,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetTokenInformation, NO_INHERITANCE,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The current user's `TOKEN_USER` (its SID points into the buffer).
fn current_user() -> io::Result<Vec<u8>> {
    // SAFETY: plain Win32 calls with valid out-pointers; the token handle is
    // closed on every path, and the buffer is sized by the first call.
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        let error = io::Error::last_os_error();
        CloseHandle(token);
        if ok == 0 {
            return Err(error);
        }
        Ok(buf)
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn check(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// Replaces `path`'s DACL with full access for the current user only and
/// stops inheriting entries from the directory.
pub fn restrict_to_current_user(path: &Path) -> io::Result<()> {
    let user = current_user()?;
    let path = wide(path);
    // SAFETY: `user` outlives every use of the SID inside it; the ACL from
    // SetEntriesInAclW is freed with LocalFree as documented.
    unsafe {
        let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: sid.cast(),
            },
        };
        let mut acl: *mut ACL = ptr::null_mut();
        check(SetEntriesInAclW(1, &entry, ptr::null(), &mut acl))?;
        let result = check(SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl,
            ptr::null(),
        ));
        LocalFree(acl.cast());
        result
    }
}

/// Whether `path`'s DACL is protected and holds exactly one entry, allowing
/// the current user (what [`restrict_to_current_user`] sets).
pub fn is_owner_only(path: &Path) -> io::Result<bool> {
    let user = current_user()?;
    let path = wide(path);
    // SAFETY: the security descriptor from GetNamedSecurityInfoW owns the
    // DACL and is freed with LocalFree after the last use.
    unsafe {
        let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        check(GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        ))?;
        let mut control = 0u16;
        let mut revision = 0u32;
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision);
        let protected = control & SE_DACL_PROTECTED != 0;
        let mut only_user = !dacl.is_null() && (*dacl).AceCount == 1;
        if only_user {
            let mut ace: *mut core::ffi::c_void = ptr::null_mut();
            only_user = GetAce(dacl, 0, &mut ace) != 0 && {
                let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
                let ace_sid = ptr::addr_of_mut!((*allowed).SidStart).cast();
                EqualSid(ace_sid, sid) != 0
            };
        }
        LocalFree(descriptor.cast());
        Ok(protected && only_user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restricted_file_has_only_the_current_user() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "secret").unwrap();
        // Freshly created files inherit the directory's entries.
        assert!(!is_owner_only(&file).unwrap());
        restrict_to_current_user(&file).unwrap();
        assert!(is_owner_only(&file).unwrap());
        // Still readable and removable by its owner.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "secret");
        std::fs::remove_file(&file).unwrap();
    }
}
