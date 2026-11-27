use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;

use rmux_os::identity::{TokenInformationBuffer, UserIdentity};
use tokio::net::windows::named_pipe::NamedPipeClient;
use windows_sys::Win32::Foundation::{LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{
    GetAce, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
    ACE_HEADER, ACL, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SYSTEM_MANDATORY_LABEL_ACE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_ACE_TYPE;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{sid_to_identity, token_user_identity, OwnedHandle};

pub(super) fn validate_named_pipe_server_identity(client: &NamedPipeClient) -> io::Result<()> {
    let server_pid = named_pipe_server_pid(client)?;
    let expected = current_process_security_identity()?;
    validate_named_pipe_server_identity_from_sources(
        server_pid,
        &expected,
        || process_security_identity(server_pid),
        || named_pipe_security_identity(client.as_raw_handle() as HANDLE),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowsSecurityIdentity {
    user: UserIdentity,
    integrity_rid: u32,
}

fn validate_named_pipe_server_identity_from_sources<ProcessIdentity, PipeIdentity>(
    server_pid: u32,
    expected: &WindowsSecurityIdentity,
    process_identity: ProcessIdentity,
    pipe_identity: PipeIdentity,
) -> io::Result<()>
where
    ProcessIdentity: FnOnce() -> io::Result<WindowsSecurityIdentity>,
    PipeIdentity: FnOnce() -> io::Result<WindowsSecurityIdentity>,
{
    match process_identity() {
        Ok(actual) => compare_named_pipe_server_identity(
            server_pid,
            "process token",
            &actual,
            expected,
            None,
        ),
        Err(process_error) => match pipe_identity() {
            Ok(actual) => compare_named_pipe_server_identity(
                server_pid,
                "pipe owner and mandatory label",
                &actual,
                expected,
                Some(process_error),
            ),
            Err(pipe_error) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "named-pipe server pid {server_pid} identity could not be verified: \
                     process token query failed: {process_error}; pipe security query failed: {pipe_error}"
                ),
            )),
        },
    }
}

fn compare_named_pipe_server_identity(
    server_pid: u32,
    source: &str,
    actual: &WindowsSecurityIdentity,
    expected: &WindowsSecurityIdentity,
    process_error: Option<io::Error>,
) -> io::Result<()> {
    if actual == expected {
        return Ok(());
    }

    let fallback_reason = process_error
        .map(|error| format!(" after process token query failed: {error}"))
        .unwrap_or_default();
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "named-pipe server pid {server_pid} {source}{fallback_reason} is {actual:?}; \
             expected current user and exact integrity {expected:?}"
        ),
    ))
}

fn named_pipe_security_identity(handle: HANDLE) -> io::Result<WindowsSecurityIdentity> {
    let mut owner = null_mut();
    let mut mandatory_label_acl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        // SAFETY: handle is a connected named-pipe client handle. Owner and
        // mandatory-label pointers remain backed by descriptor until return.
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            &mut mandatory_label_acl,
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let _descriptor = LocalSecurityDescriptor(descriptor);
    if owner.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned a null named-pipe owner SID",
        ));
    }
    if mandatory_label_acl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned no named-pipe mandatory label",
        ));
    }

    // Windows enforces the mandatory label on the pipe object itself. A lower
    // integrity process cannot assign a higher label without relabel privilege,
    // so owner SID plus the exact label is a safe fallback when a cross-logon
    // token DACL prevents OpenProcessToken.
    Ok(WindowsSecurityIdentity {
        user: sid_to_identity(owner)?,
        integrity_rid: mandatory_integrity_rid_from_acl(mandatory_label_acl)?,
    })
}

fn mandatory_integrity_rid_from_acl(acl: *const ACL) -> io::Result<u32> {
    let ace_count = unsafe {
        // SAFETY: acl is backed by a live security descriptor from GetSecurityInfo.
        (*acl).AceCount
    };
    for index in 0..u32::from(ace_count) {
        let mut ace = null_mut();
        let ok = unsafe {
            // SAFETY: index is within the AceCount reported by this ACL.
            GetAce(acl, index, &mut ace)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe {
            // SAFETY: every validated Windows ACE starts with ACE_HEADER.
            &*(ace.cast::<ACE_HEADER>())
        };
        if u32::from(header.AceType) != SYSTEM_MANDATORY_LABEL_ACE_TYPE {
            continue;
        }
        if usize::from(header.AceSize) < std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows named-pipe mandatory-label ACE is truncated",
            ));
        }
        let mandatory_label = unsafe {
            // SAFETY: the ACE type and size match SYSTEM_MANDATORY_LABEL_ACE.
            &*(ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>())
        };
        let sid = std::ptr::addr_of!(mandatory_label.SidStart)
            .cast_mut()
            .cast();
        return integrity_rid_from_sid(sid);
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Windows named pipe has no mandatory-label ACE",
    ))
}

fn named_pipe_server_pid(client: &NamedPipeClient) -> io::Result<u32> {
    let mut pid = 0;
    let ok = unsafe {
        // SAFETY: client is a connected named-pipe client handle and pid is a valid out pointer.
        GetNamedPipeServerProcessId(client.as_raw_handle() as HANDLE, &mut pid)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

fn process_security_identity(pid: u32) -> io::Result<WindowsSecurityIdentity> {
    let process = open_process_for_token_query(pid)?;
    let token = process_token(process.get())?;
    token_security_identity(token.get())
}

fn current_process_security_identity() -> io::Result<WindowsSecurityIdentity> {
    let token = process_token(unsafe {
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle for this process.
        GetCurrentProcess()
    })?;
    token_security_identity(token.get())
}

fn process_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    let ok = unsafe {
        // SAFETY: process is a live process handle and token is a valid out pointer.
        OpenProcessToken(process, TOKEN_QUERY, &mut token)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(token))
}

fn token_security_identity(token: HANDLE) -> io::Result<WindowsSecurityIdentity> {
    Ok(WindowsSecurityIdentity {
        user: token_user_identity(token)?,
        integrity_rid: token_integrity_rid(token)?,
    })
}

fn open_process_for_token_query(pid: u32) -> io::Result<OwnedHandle> {
    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(handle))
}

fn token_integrity_rid(token: HANDLE) -> io::Result<u32> {
    let mut needed = 0_u32;
    unsafe {
        // SAFETY: This first call intentionally requests the required byte count.
        GetTokenInformation(token, TokenIntegrityLevel, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut buffer = TokenInformationBuffer::<TOKEN_MANDATORY_LABEL>::new(needed)?;
    let buffer_len = buffer.byte_len();
    let ok = unsafe {
        // SAFETY: buffer is writable for the aligned byte count allocated above.
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            buffer.as_mut_ptr(),
            buffer_len,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let mandatory_label = unsafe {
        // SAFETY: A successful TokenIntegrityLevel query initializes
        // TOKEN_MANDATORY_LABEL and its SID remains backed by `buffer`.
        buffer.assume_init_header()
    };
    integrity_rid_from_sid(mandatory_label.Label.Sid)
}

fn integrity_rid_from_sid(sid: *mut core::ffi::c_void) -> io::Result<u32> {
    if sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned a null integrity SID",
        ));
    }
    let count_ptr = unsafe {
        // SAFETY: sid comes from a Windows integrity-label structure.
        GetSidSubAuthorityCount(sid)
    };
    if count_ptr.is_null() {
        return Err(io::Error::last_os_error());
    }
    let count = unsafe { *count_ptr };
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows integrity SID has no subauthorities",
        ));
    }
    let rid_ptr = unsafe {
        // SAFETY: count is non-zero and the final subauthority index is valid.
        GetSidSubAuthority(sid, u32::from(count - 1))
    };
    if rid_ptr.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { *rid_ptr })
}

struct LocalSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: descriptor came from GetSecurityInfo.
                LocalFree(self.0.cast());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{endpoint_for_label, LocalListener};

    fn identity(user: &str, integrity_rid: u32) -> WindowsSecurityIdentity {
        WindowsSecurityIdentity {
            user: UserIdentity::Sid(user.into()),
            integrity_rid,
        }
    }

    #[tokio::test]
    async fn pipe_security_identity_matches_current_process_identity() -> io::Result<()> {
        let endpoint = endpoint_for_label(format!("pipe-security-{}", std::process::id()))?;
        let _listener = LocalListener::bind(&endpoint)?;
        let client = super::super::connect_windows_pipe(endpoint.as_pipe_name()).await?;

        let from_pipe = named_pipe_security_identity(client.as_raw_handle() as HANDLE)?;
        let current = current_process_security_identity()?;

        assert_eq!(from_pipe, current);
        Ok(())
    }

    #[test]
    fn server_identity_accepts_matching_user_and_exact_integrity() {
        let expected = identity("S-1-5-21-1000", 0x3000);

        validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || Ok(expected.clone()),
            || panic!("pipe fallback must not run for a readable process token"),
        )
        .expect("matching user and integrity should be accepted");
    }

    #[test]
    fn server_identity_rejects_same_user_at_lower_integrity() {
        let expected = identity("S-1-5-21-1000", 0x3000);
        let error = validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || Ok(identity("S-1-5-21-1000", 0x2000)),
            || panic!("pipe fallback must not override a process-token mismatch"),
        )
        .expect_err("same-user lower-integrity server must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("exact integrity"));
    }

    #[test]
    fn server_identity_rejects_different_user_at_matching_integrity() {
        let expected = identity("S-1-5-21-1000", 0x2000);
        let error = validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || Ok(identity("S-1-5-21-2000", 0x2000)),
            || panic!("pipe fallback must not override a process-token mismatch"),
        )
        .expect_err("different-user server must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("S-1-5-21-2000"));
    }

    #[test]
    fn server_identity_accepts_exact_pipe_security_when_process_token_is_unverifiable() {
        let expected = identity("S-1-5-21-1000", 0x2000);
        validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "OpenProcess denied",
                ))
            },
            || Ok(expected.clone()),
        )
        .expect("exact owner and mandatory label should authenticate the pipe");
    }

    #[test]
    fn server_identity_rejects_lower_integrity_pipe_after_process_query_denial() {
        let expected = identity("S-1-5-21-1000", 0x3000);
        let error = validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "OpenProcess denied",
                ))
            },
            || Ok(identity("S-1-5-21-1000", 0x2000)),
        )
        .expect_err("lower-integrity pipe must not authenticate an elevated endpoint");

        let message = error.to_string();
        assert!(message.contains("pipe owner and mandatory label"));
        assert!(message.contains("OpenProcess denied"));
    }

    #[test]
    fn server_identity_fails_closed_when_both_identity_sources_are_unverifiable() {
        let expected = identity("S-1-5-21-1000", 0x2000);
        let error = validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "token denied",
                ))
            },
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "label denied",
                ))
            },
        )
        .expect_err("unverifiable server identity must fail closed");

        let message = error.to_string();
        assert!(message.contains("process token query failed"));
        assert!(message.contains("pipe security query failed"));
    }

    #[test]
    fn current_process_identity_matches_pid_token_lookup() {
        let current = current_process_security_identity().expect("current process token");
        let by_pid = process_security_identity(std::process::id()).expect("process token by pid");

        assert_eq!(by_pid, current);
    }
}
