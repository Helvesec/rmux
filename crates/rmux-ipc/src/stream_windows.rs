use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle as OwnedWindowsHandle};
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rmux_os::identity::{TokenInformationBuffer, UserIdentity};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA,
    ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, RevertToSelf,
    TokenIntegrityLevel, TokenUser, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
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
    GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken,
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
    inner: Option<NamedPipeClient>,
    runtime: Option<tokio::runtime::Runtime>,
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
    pub fn into_async_parts(mut self) -> (NamedPipeClient, tokio::runtime::Runtime) {
        let inner = self
            .inner
            .take()
            .expect("blocking named-pipe stream must own its client");
        let runtime = self
            .runtime
            .take()
            .expect("blocking named-pipe stream must own its runtime");
        (inner, runtime)
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
        spawn_peer_identity_query(stream, |handle| {
            peer_identity_from_handle(handle.as_raw_handle() as HANDLE)
        })?
        .await
        .map_err(|error| io::Error::other(format!("Windows peer identity task failed: {error}")))?
    }
}

fn spawn_peer_identity_query<Query>(
    stream: &LocalStream,
    query: Query,
) -> io::Result<tokio::task::JoinHandle<io::Result<PeerIdentity>>>
where
    Query: FnOnce(OwnedWindowsHandle) -> io::Result<PeerIdentity> + Send + 'static,
{
    let handle = stream.as_handle().try_clone_to_owned()?;
    Ok(tokio::task::spawn_blocking(move || query(handle)))
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
                    inner: Some(inner),
                    runtime: Some(runtime),
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
/// verifies that the server belongs to the current Windows user at the exact
/// same mandatory integrity level.
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
        let timeout = self.read_timeout()?;
        let runtime = self
            .runtime
            .as_ref()
            .expect("blocking named-pipe stream must own its runtime");
        let inner = self
            .inner
            .as_mut()
            .expect("blocking named-pipe stream must own its client");
        match timeout {
            Some(timeout) => runtime.block_on(async {
                tokio::time::timeout(timeout, inner.read(buf))
                    .await
                    .map_err(|_| timeout_error("read", timeout))?
            }),
            None => runtime.block_on(inner.read(buf)),
        }
    }
}

impl Write for BlockingLocalStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let timeout = self.write_timeout();
        let runtime = self
            .runtime
            .as_ref()
            .expect("blocking named-pipe stream must own its runtime");
        let inner = self
            .inner
            .as_mut()
            .expect("blocking named-pipe stream must own its client");
        match timeout {
            Some(timeout) => runtime.block_on(async {
                tokio::time::timeout(timeout, inner.write(buf))
                    .await
                    .map_err(|_| timeout_error("write", timeout))?
            }),
            None => runtime.block_on(inner.write(buf)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let timeout = self.write_timeout();
        let runtime = self
            .runtime
            .as_ref()
            .expect("blocking named-pipe stream must own its runtime");
        let inner = self
            .inner
            .as_mut()
            .expect("blocking named-pipe stream must own its client");
        match timeout {
            Some(timeout) => runtime.block_on(async {
                tokio::time::timeout(timeout, inner.flush())
                    .await
                    .map_err(|_| timeout_error("flush", timeout))?
            }),
            None => runtime.block_on(inner.flush()),
        }
    }
}

impl Drop for BlockingLocalStream {
    fn drop(&mut self) {
        drop(self.inner.take());
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
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
    let expected = current_process_security_identity()?;
    validate_named_pipe_server_identity_from_source(server_pid, &expected, || {
        process_security_identity(server_pid)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowsSecurityIdentity {
    user: UserIdentity,
    integrity_rid: u32,
}

fn validate_named_pipe_server_identity_from_source<ProcessIdentity>(
    server_pid: u32,
    expected: &WindowsSecurityIdentity,
    process_identity: ProcessIdentity,
) -> io::Result<()>
where
    ProcessIdentity: FnOnce() -> io::Result<WindowsSecurityIdentity>,
{
    let actual = process_identity().map_err(|error| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "named-pipe server pid {server_pid} process token identity and integrity \
                 could not be verified: {error}"
            ),
        )
    })?;
    compare_named_pipe_server_identity(server_pid, &actual, expected)
}

fn compare_named_pipe_server_identity(
    server_pid: u32,
    actual: &WindowsSecurityIdentity,
    expected: &WindowsSecurityIdentity,
) -> io::Result<()> {
    if actual == expected {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "named-pipe server pid {server_pid} process token is {actual:?}; \
             expected current user and exact integrity {expected:?}"
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

    let mut buffer = TokenInformationBuffer::<TOKEN_USER>::new(needed)?;
    let buffer_len = buffer.byte_len();
    let ok = unsafe {
        // SAFETY: buffer is writable for the aligned byte count allocated above.
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr(),
            buffer_len,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let token_user = unsafe {
        // SAFETY: A successful TokenUser query initializes a valid TOKEN_USER
        // header and its SID remains backed by `buffer` for this call.
        buffer.assume_init_header()
    };
    sid_to_identity(token_user.User.Sid)
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
        // SAFETY: sid comes from a successfully queried TOKEN_MANDATORY_LABEL.
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
    use crate::endpoint_for_label;
    use std::sync::mpsc;
    use tokio::net::windows::named_pipe::ServerOptions;

    fn identity(user: &str, integrity_rid: u32) -> WindowsSecurityIdentity {
        WindowsSecurityIdentity {
            user: UserIdentity::Sid(user.into()),
            integrity_rid,
        }
    }

    #[tokio::test]
    async fn peer_identity_query_owns_handle_after_accept_future_drop() -> io::Result<()> {
        let endpoint = endpoint_for_label(format!("peer-identity-cancel-{}", std::process::id()))?;
        let server = ServerOptions::new().create(endpoint.as_pipe_name())?;
        let _client = connect_windows_pipe(endpoint.as_pipe_name()).await?;
        server.connect().await?;

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let query = spawn_peer_identity_query(&server, move |handle| {
            entered_tx
                .send(())
                .map_err(|error| io::Error::other(error.to_string()))?;
            release_rx
                .recv_timeout(Duration::from_secs(2))
                .map_err(io::Error::other)?;
            peer_identity_from_handle(handle.as_raw_handle() as HANDLE)
        })?;

        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(io::Error::other)?;
        // Dropping the stream models cancellation of `LocalListener::accept`
        // after its blocking identity worker has started.
        drop(server);
        release_tx.send(()).map_err(io::Error::other)?;

        let peer = query.await.map_err(io::Error::other)??;
        assert_eq!(peer.pid, std::process::id());
        Ok(())
    }

    #[test]
    fn server_identity_accepts_matching_user_and_exact_integrity() {
        let expected = identity("S-1-5-21-1000", 0x3000);

        validate_named_pipe_server_identity_from_source(42, &expected, || Ok(expected.clone()))
            .expect("matching user and integrity should be accepted");
    }

    #[test]
    fn server_identity_rejects_same_user_at_lower_integrity() {
        let expected = identity("S-1-5-21-1000", 0x3000);
        let error = validate_named_pipe_server_identity_from_source(42, &expected, || {
            Ok(identity("S-1-5-21-1000", 0x2000))
        })
        .expect_err("same-user lower-integrity server must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("exact integrity"));
    }

    #[test]
    fn server_identity_rejects_different_user_at_matching_integrity() {
        let expected = identity("S-1-5-21-1000", 0x2000);
        let error = validate_named_pipe_server_identity_from_source(42, &expected, || {
            Ok(identity("S-1-5-21-2000", 0x2000))
        })
        .expect_err("different-user server must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("S-1-5-21-2000"));
    }

    #[test]
    fn server_identity_fails_closed_when_process_token_is_unverifiable() {
        let expected = identity("S-1-5-21-1000", 0x2000);
        let error = validate_named_pipe_server_identity_from_source(42, &expected, || {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "OpenProcess denied",
            ))
        })
        .expect_err("unverifiable server identity must be rejected");

        let message = error.to_string();
        assert!(message.contains("identity and integrity could not be verified"));
        assert!(message.contains("OpenProcess denied"));
    }

    #[test]
    fn current_process_identity_matches_pid_token_lookup() {
        let current = current_process_security_identity().expect("current process token");
        let by_pid = process_security_identity(std::process::id()).expect("process token by pid");

        assert_eq!(by_pid, current);
    }
}
