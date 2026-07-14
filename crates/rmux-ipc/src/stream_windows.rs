use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rmux_os::identity::{IdentityResolver, UserIdentity};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA,
    ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, RevertToSelf, TokenUser, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_CREATE_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, ImpersonateNamedPipeClient,
    PeekNamedPipe, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::PeerIdentity;
use crate::LocalEndpoint;

const RMUX_NAMED_PIPE_CLIENT_ACCESS: u32 =
    FILE_GENERIC_READ | (FILE_GENERIC_WRITE & !FILE_CREATE_PIPE_INSTANCE);
const RMUX_NAMED_PIPE_CLIENT_FLAGS: u32 =
    SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT | FILE_FLAG_OVERLAPPED;
const WINDOWS_SYNTHETIC_UID: u32 = 0;

/// Async local byte stream used by the server runtime.
pub type LocalStream = NamedPipeServer;

/// Async named-pipe client returned by the verified Windows connector.
pub type WindowsPipeClient = NamedPipeClient;

/// Blocking local byte stream used by the CLI.
pub struct BlockingLocalStream {
    inner: NamedPipeClient,
    runtime: tokio::runtime::Runtime,
    timeouts: Mutex<IoTimeouts>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IoTimeouts {
    read: Option<Duration>,
    write: Option<Duration>,
}

impl std::fmt::Debug for BlockingLocalStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BlockingLocalStream(named pipe)")
    }
}

impl BlockingLocalStream {
    /// Consumes the blocking wrapper and returns its Tokio pipe client plus
    /// the runtime that owns its I/O driver.
    pub fn into_async_parts(self) -> (NamedPipeClient, tokio::runtime::Runtime) {
        (self.inner, self.runtime)
    }

    /// Returns the current read timeout for detached RPC reads.
    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(self.timeouts.lock().expect("named-pipe timeouts").read)
    }

    /// Sets the current read timeout for detached RPC reads.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.timeouts.lock().expect("named-pipe timeouts").read = timeout;
        Ok(())
    }

    /// Sets the current write timeout for detached RPC writes.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.timeouts.lock().expect("named-pipe timeouts").write = timeout;
        Ok(())
    }

    fn write_timeout(&self) -> Option<Duration> {
        self.timeouts.lock().expect("named-pipe timeouts").write
    }
}

impl PeerIdentity {
    pub(crate) async fn from_windows_pipe(stream: &LocalStream) -> io::Result<Self> {
        let handle = stream.as_raw_handle() as isize;
        tokio::task::spawn_blocking(move || peer_identity_from_handle(handle as HANDLE))
            .await
            .map_err(|error| {
                io::Error::other(format!("Windows peer identity task failed: {error}"))
            })?
    }
}

/// Connects a blocking client stream to a local endpoint.
pub fn connect_blocking(
    endpoint: &LocalEndpoint,
    timeout: Duration,
) -> io::Result<BlockingLocalStream> {
    let pipe_name = endpoint.as_pipe_name().to_owned();
    if named_pipe_is_definitely_absent(&pipe_name) {
        return Err(io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    let deadline = Instant::now() + timeout;
    loop {
        match runtime.block_on(connect_windows_pipe(&pipe_name)) {
            Ok(inner) => {
                return Ok(BlockingLocalStream {
                    inner,
                    runtime,
                    timeouts: Mutex::new(IoTimeouts::default()),
                });
            }
            Err(error) if connect_retryable(&error) => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "timed out after {}s connecting to '{}'",
                            timeout.as_secs_f32(),
                            endpoint.as_path().display()
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

fn named_pipe_is_definitely_absent(pipe_name: &std::ffi::OsStr) -> bool {
    let wide = pipe_name
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let available = unsafe {
        // SAFETY: `wide` is a nul-terminated UTF-16 pipe name. A zero timeout
        // only asks the kernel whether any matching pipe instance exists.
        WaitNamedPipeW(wide.as_ptr(), 0)
    };
    if available != 0 {
        return false;
    }

    matches!(
        io::Error::last_os_error().raw_os_error(),
        Some(code) if code == ERROR_FILE_NOT_FOUND as i32
    )
}

pub(super) async fn wait_for_peer_close_impl(stream: &LocalStream) -> io::Result<()> {
    loop {
        if let Err(error) = stream.readable().await {
            if is_peer_disconnect(&error) {
                return Ok(());
            }
            return Err(error);
        }

        let mut available = 0_u32;
        let ok = unsafe {
            // SAFETY: `stream` is a connected named-pipe server handle and
            // `available` is a valid out pointer. Passing a null buffer peeks
            // byte counts only and does not consume protocol data.
            PeekNamedPipe(
                stream.as_raw_handle() as HANDLE,
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if is_peer_disconnect(&error) {
                return Ok(());
            }
            return Err(error);
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn connect_retryable(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == ERROR_PIPE_BUSY as i32
            || code == ERROR_PIPE_NOT_CONNECTED as i32
            || code == ERROR_NO_DATA as i32
    )
}

/// Opens a Tokio named-pipe client with RMUX's restricted access rights and
/// verifies that the server belongs to the current Windows user.
///
/// Callers should use this boundary instead of Tokio's unrestricted
/// `ClientOptions::open` so every asynchronous RMUX client applies the same
/// server-identity policy as [`connect_blocking`].
pub async fn connect_windows_pipe(pipe_name: &std::ffi::OsStr) -> io::Result<WindowsPipeClient> {
    let client = open_named_pipe_client_handle(pipe_name)?;
    validate_named_pipe_server_identity(&client)?;
    Ok(client)
}

fn open_named_pipe_client_handle(pipe_name: &std::ffi::OsStr) -> io::Result<NamedPipeClient> {
    let wide = pipe_name
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        // SAFETY: `wide` is a nul-terminated UTF-16 pipe name. The client only
        // needs read/write/synchronize/read-control rights; it must not request
        // FILE_CREATE_PIPE_INSTANCE, which is a server-side named-pipe right.
        CreateFileW(
            wide.as_ptr(),
            RMUX_NAMED_PIPE_CLIENT_ACCESS,
            0,
            null(),
            OPEN_EXISTING,
            RMUX_NAMED_PIPE_CLIENT_FLAGS,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    unsafe {
        // SAFETY: the handle came from CreateFileW with FILE_FLAG_OVERLAPPED
        // and ownership is transferred to Tokio's named-pipe wrapper.
        NamedPipeClient::from_raw_handle(handle.cast())
    }
}

impl Read for BlockingLocalStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.read_timeout()? {
            Some(timeout) => self.runtime.block_on(async {
                tokio::time::timeout(timeout, self.inner.read(buf))
                    .await
                    .map_err(|_| timeout_error("read", timeout))?
            }),
            None => self.runtime.block_on(self.inner.read(buf)),
        }
    }
}

impl Write for BlockingLocalStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.write_timeout() {
            Some(timeout) => self.runtime.block_on(async {
                tokio::time::timeout(timeout, self.inner.write(buf))
                    .await
                    .map_err(|_| timeout_error("write", timeout))?
            }),
            None => self.runtime.block_on(self.inner.write(buf)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.write_timeout() {
            Some(timeout) => self.runtime.block_on(async {
                tokio::time::timeout(timeout, self.inner.flush())
                    .await
                    .map_err(|_| timeout_error("flush", timeout))?
            }),
            None => self.runtime.block_on(self.inner.flush()),
        }
    }
}

fn timeout_error(operation: &str, timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out after {}s waiting for named-pipe {operation}",
            timeout.as_secs_f32()
        ),
    )
}

pub(super) fn is_peer_disconnect(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::NotFound
    ) {
        return true;
    }
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_BROKEN_PIPE as i32
                || code == ERROR_PIPE_NOT_CONNECTED as i32
                || code == ERROR_NO_DATA as i32
                || code == ERROR_FILE_NOT_FOUND as i32
    )
}

fn peer_identity_from_handle(handle: HANDLE) -> io::Result<PeerIdentity> {
    let pid = named_pipe_client_pid(handle)?;
    let user = named_pipe_client_user(handle)?;
    Ok(PeerIdentity {
        pid,
        // Windows has no Unix uid. Authorization and display use `user`
        // (the peer SID); this synthetic value only satisfies shared protocol
        // fields that remain Unix-shaped.
        uid: WINDOWS_SYNTHETIC_UID,
        user,
    })
}

fn validate_named_pipe_server_identity(client: &NamedPipeClient) -> io::Result<()> {
    let server_pid = named_pipe_server_pid(client)?;
    let expected = IdentityResolver::current()?;
    validate_named_pipe_server_identity_from_sources(
        server_pid,
        &expected,
        || process_user_identity(server_pid),
        || named_pipe_owner_identity(client.as_raw_handle() as HANDLE),
    )
}

fn validate_named_pipe_server_identity_from_sources<ProcessUser, PipeOwner>(
    server_pid: u32,
    expected: &UserIdentity,
    process_user: ProcessUser,
    pipe_owner: PipeOwner,
) -> io::Result<()>
where
    ProcessUser: FnOnce() -> io::Result<UserIdentity>,
    PipeOwner: FnOnce() -> io::Result<UserIdentity>,
{
    match process_user() {
        Ok(actual) => compare_named_pipe_server_identity(
            server_pid,
            "process token",
            actual,
            expected,
            None,
        ),
        Err(process_error) => match pipe_owner() {
            Ok(actual) => compare_named_pipe_server_identity(
                server_pid,
                "pipe owner",
                actual,
                expected,
                Some(process_error),
            ),
            Err(owner_error) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "named-pipe server pid {server_pid} identity could not be verified: \
                     process token query failed: {process_error}; pipe owner query failed: {owner_error}"
                ),
            )),
        },
    }
}

fn compare_named_pipe_server_identity(
    server_pid: u32,
    source: &str,
    actual: UserIdentity,
    expected: &UserIdentity,
    fallback_reason: Option<io::Error>,
) -> io::Result<()> {
    if &actual == expected {
        return Ok(());
    }

    let reason = fallback_reason
        .map(|error| format!(" after process token query failed: {error}"))
        .unwrap_or_default();
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "named-pipe server pid {server_pid} {source}{reason} is owned by {actual:?}; expected {expected:?}"
        ),
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

fn process_user_identity(pid: u32) -> io::Result<UserIdentity> {
    let process = open_process_for_token_query(pid)?;
    let mut token = null_mut();
    let ok = unsafe {
        // SAFETY: process is a live process handle and token is a valid out pointer.
        OpenProcessToken(process.get(), TOKEN_QUERY, &mut token)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    token_user_identity(token.get())
}

fn named_pipe_owner_identity(handle: HANDLE) -> io::Result<UserIdentity> {
    let mut owner = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        // SAFETY: handle is a connected named-pipe client handle. GetSecurityInfo
        // initializes `owner` to point inside `descriptor`, which is freed below.
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
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

    sid_to_identity(owner)
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

fn named_pipe_client_pid(handle: HANDLE) -> io::Result<u32> {
    let mut pid = 0;
    let ok = unsafe {
        // SAFETY: handle is a connected named-pipe server handle and pid is a valid out pointer.
        GetNamedPipeClientProcessId(handle, &mut pid)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

fn named_pipe_client_user(handle: HANDLE) -> io::Result<UserIdentity> {
    let ok = unsafe {
        // SAFETY: handle is a connected named-pipe server handle. RevertGuard
        // below restores this short-lived worker thread token after querying the client token.
        ImpersonateNamedPipeClient(handle)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let _guard = RevertGuard;

    let mut token = null_mut();
    let ok = unsafe {
        // SAFETY: GetCurrentThread returns a valid pseudo-handle and token is a valid out pointer.
        OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    token_user_identity(token.get())
}

fn token_user_identity(token: HANDLE) -> io::Result<UserIdentity> {
    let mut needed = 0;
    unsafe {
        // SAFETY: This first call intentionally requests the required byte count.
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut buffer = vec![0_u8; usize::try_from(needed).map_err(|_| io::ErrorKind::InvalidData)?];
    let ok = unsafe {
        // SAFETY: buffer is writable for the byte count reported by Windows.
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let token_user = unsafe {
        // SAFETY: A successful TokenUser query initializes TOKEN_USER at the buffer start.
        &*(buffer.as_ptr().cast::<TOKEN_USER>())
    };
    sid_to_identity(token_user.User.Sid)
}

fn sid_to_identity(sid: *mut core::ffi::c_void) -> io::Result<UserIdentity> {
    let mut sid_string = null_mut();
    let ok = unsafe {
        // SAFETY: sid comes from TOKEN_USER and sid_string is freed with LocalFree on success.
        ConvertSidToStringSidW(sid, &mut sid_string)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let value = wide_ptr_to_string(sid_string.cast_const());
    unsafe {
        // SAFETY: sid_string was allocated by ConvertSidToStringSidW.
        LocalFree(sid_string.cast());
    }
    value.map(|sid| UserIdentity::Sid(sid.into_boxed_str()))
}

fn wide_ptr_to_string(ptr: *const u16) -> io::Result<String> {
    if ptr.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned a null SID string",
        ));
    }

    let mut len = 0;
    unsafe {
        // SAFETY: Windows returns a nul-terminated UTF-16 string on success.
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16(std::slice::from_raw_parts(ptr, len)).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid UTF-16 SID string: {error}"),
            )
        })
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn get(&self) -> HANDLE {
        self.0
    }
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

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                // SAFETY: self.0 is a handle returned by OpenThreadToken.
                CloseHandle(self.0);
            }
        }
    }
}

struct RevertGuard;

impl Drop for RevertGuard {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: this short-lived worker thread may have been impersonating;
            // there is no useful recovery path during Drop.
            RevertToSelf();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(value: &str) -> UserIdentity {
        UserIdentity::Sid(value.into())
    }

    #[test]
    fn server_identity_accepts_matching_process_token() {
        let expected = sid("S-1-5-21-1000");

        validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || Ok(expected.clone()),
            || panic!("pipe owner fallback should not be used when the process token is readable"),
        )
        .expect("matching process token should be accepted");
    }

    #[test]
    fn server_identity_falls_back_to_matching_pipe_owner() {
        let expected = sid("S-1-5-21-1000");

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
        .expect("matching pipe owner should verify the server when process token lookup is denied");
    }

    #[test]
    fn server_identity_rejects_mismatched_pipe_owner_after_process_denial() {
        let expected = sid("S-1-5-21-1000");
        let error = validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "OpenProcess denied",
                ))
            },
            || Ok(sid("S-1-5-21-2000")),
        )
        .expect_err("mismatched pipe owner must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("pipe owner"));
    }

    #[test]
    fn server_identity_reports_both_query_failures() {
        let expected = sid("S-1-5-21-1000");
        let error = validate_named_pipe_server_identity_from_sources(
            42,
            &expected,
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "OpenProcess denied",
                ))
            },
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "GetSecurityInfo denied",
                ))
            },
        )
        .expect_err("unverifiable server identity must be rejected");

        let message = error.to_string();
        assert!(message.contains("process token query failed"));
        assert!(message.contains("pipe owner query failed"));
    }
}
