//! Local stream handles.

#[cfg(windows)]
use std::ffi::OsString;
use std::io;
#[cfg(windows)]
use std::io::{Read, Write};
#[cfg(all(unix, not(target_os = "linux")))]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(unix)]
use std::path::Path;
#[cfg(windows)]
use std::ptr::null_mut;
use std::time::Duration;
use std::time::Instant;

use crate::LocalEndpoint;
use rmux_os::identity::UserIdentity;

#[cfg(unix)]
use rustix::net::RecvFlags;
#[cfg(windows)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_NOT_CONNECTED, HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
#[cfg(windows)]
use windows_sys::Win32::Security::{
    GetTokenInformation, RevertToSelf, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeClientProcessId, ImpersonateNamedPipeClient, PeekNamedPipe,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

/// Identity of a connected local peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Peer process id.
    pub pid: u32,
    /// Peer Unix user id.
    pub uid: u32,
    /// Platform user identity for the peer.
    pub user: UserIdentity,
}

/// Async local byte stream used by the server runtime.
#[cfg(unix)]
pub type LocalStream = tokio::net::UnixStream;

/// Blocking local byte stream used by the CLI.
#[cfg(unix)]
pub type BlockingLocalStream = std::os::unix::net::UnixStream;

/// Async local byte stream default_value for Windows until named pipes are added.
#[cfg(windows)]
pub type LocalStream = tokio::net::windows::named_pipe::NamedPipeServer;

/// Blocking local byte stream used by the CLI.
#[cfg(windows)]
pub struct BlockingLocalStream {
    inner: NamedPipeClient,
    runtime: tokio::runtime::Runtime,
}

/// Waits for the connected local peer to disappear without consuming protocol bytes.
pub async fn wait_for_peer_close(stream: &LocalStream) -> io::Result<()> {
    wait_for_peer_close_impl(stream).await
}

#[cfg(unix)]
async fn wait_for_peer_close_impl(stream: &LocalStream) -> io::Result<()> {
    loop {
        if let Err(error) = stream.readable().await {
            if is_peer_disconnect(&error) {
                return Ok(());
            }
            return Err(error);
        }
        let mut probe = [0_u8; 1];

        match rustix::net::recv(stream, &mut probe, RecvFlags::PEEK) {
            Ok((_initialized, 0)) => return Ok(()),
            Ok((_initialized, _available)) => return std::future::pending().await,
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
            Err(rustix::io::Errno::PIPE | rustix::io::Errno::CONNRESET) => return Ok(()),
            Err(error) => return Err(io::Error::from(error)),
        }
    }
}

#[cfg(windows)]
async fn wait_for_peer_close_impl(stream: &LocalStream) -> io::Result<()> {
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
        if available > 0 {
            return std::future::pending().await;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn is_peer_disconnect(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    ) {
        return true;
    }
    #[cfg(windows)]
    {
        if matches!(
            error.raw_os_error(),
            Some(code)
                if code == ERROR_BROKEN_PIPE as i32
                    || code == ERROR_PIPE_NOT_CONNECTED as i32
                    || code == ERROR_NO_DATA as i32
        ) {
            return true;
        }
    }
    false
}

#[cfg(windows)]
impl std::fmt::Debug for BlockingLocalStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BlockingLocalStream(named pipe)")
    }
}

#[cfg(windows)]
impl BlockingLocalStream {
    /// Consumes the blocking wrapper and returns its Tokio pipe client plus
    /// the runtime that owns its I/O driver.
    pub fn into_async_parts(self) -> (NamedPipeClient, tokio::runtime::Runtime) {
        (self.inner, self.runtime)
    }
}

#[cfg(unix)]
impl PeerIdentity {
    pub(crate) fn from_unix_stream(stream: &LocalStream) -> io::Result<Self> {
        let credentials = stream.peer_cred()?;
        let pid = credentials
            .pid()
            .ok_or_else(|| io::Error::other("unix peer credentials did not include a pid"))?;
        let uid = credentials.uid();
        let pid = u32::try_from(pid)
            .map_err(|_| io::Error::other(format!("invalid unix peer pid {pid}")))?;
        Ok(Self {
            pid,
            uid,
            user: UserIdentity::Uid(uid),
        })
    }
}

#[cfg(windows)]
impl PeerIdentity {
    pub(crate) fn from_windows_pipe(stream: &LocalStream) -> io::Result<Self> {
        let handle = stream.as_raw_handle() as HANDLE;
        let pid = named_pipe_client_pid(handle)?;
        let user = named_pipe_client_user(handle)?;
        Ok(Self { pid, uid: 0, user })
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
fn named_pipe_client_user(handle: HANDLE) -> io::Result<UserIdentity> {
    let ok = unsafe {
        // SAFETY: handle is a connected named-pipe server handle. RevertGuard
        // below restores the server thread token after querying the client token.
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

#[cfg(windows)]
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

#[cfg(windows)]
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

#[cfg(windows)]
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

#[cfg(windows)]
struct OwnedHandle(HANDLE);

#[cfg(windows)]
impl OwnedHandle {
    fn get(&self) -> HANDLE {
        self.0
    }
}

#[cfg(windows)]
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

#[cfg(windows)]
struct RevertGuard;

#[cfg(windows)]
impl Drop for RevertGuard {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: this thread may have been impersonating; RevertToSelf is idempotent enough
            // for cleanup here and there is no useful recovery path during Drop.
            RevertToSelf();
        }
    }
}

#[cfg(unix)]
/// Connects a blocking client stream to a local endpoint.
pub fn connect_blocking(
    endpoint: &LocalEndpoint,
    timeout: Duration,
) -> io::Result<BlockingLocalStream> {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    use rustix::net::sockopt::socket_error;
    use rustix::net::{
        connect as socket_connect, socket_with, AddressFamily, SocketAddrUnix, SocketType,
    };

    let socket_path = endpoint.as_path();
    let address = SocketAddrUnix::new(socket_path)?;
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        socket_creation_flags(),
        None,
    )?;
    configure_socket_for_connect(&socket)?;

    match socket_connect(&socket, &address) {
        Ok(()) => {}
        Err(rustix::io::Errno::INPROGRESS | rustix::io::Errno::WOULDBLOCK) => {
            wait_for_connect_completion(socket_path, timeout, |remaining| {
                let poll_timeout = Timespec {
                    tv_sec: remaining.as_secs() as i64,
                    tv_nsec: remaining.subsec_nanos().into(),
                };
                let mut fds = [PollFd::new(
                    &socket,
                    PollFlags::OUT | PollFlags::ERR | PollFlags::HUP,
                )];

                match poll(&mut fds, Some(&poll_timeout)) {
                    Ok(0) => Ok(ConnectProgress::Pending),
                    Ok(_) => Ok(ConnectProgress::Ready),
                    Err(rustix::io::Errno::INTR) => Ok(ConnectProgress::Pending),
                    Err(error) => Err(error.into()),
                }
            })?;
        }
        Err(error) => return Err(error.into()),
    }

    match socket_error(&socket)? {
        Ok(()) => {}
        Err(error) => return Err(error.into()),
    }

    let stream = BlockingLocalStream::from(socket);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(windows)]
/// Connects a blocking client stream to a local endpoint.
pub fn connect_blocking(
    endpoint: &LocalEndpoint,
    timeout: Duration,
) -> io::Result<BlockingLocalStream> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let deadline = Instant::now() + timeout;
    let pipe_name = endpoint.as_pipe_name().to_owned();
    loop {
        match runtime.block_on(open_named_pipe_client(&pipe_name)) {
            Ok(inner) => return Ok(BlockingLocalStream { inner, runtime }),
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

#[cfg(windows)]
fn connect_retryable(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
}

#[cfg(windows)]
async fn open_named_pipe_client(pipe_name: &OsString) -> io::Result<NamedPipeClient> {
    ClientOptions::new().open(pipe_name)
}

#[cfg(windows)]
impl Read for BlockingLocalStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.runtime.block_on(self.inner.read(buf))
    }
}

#[cfg(windows)]
impl Write for BlockingLocalStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.runtime.block_on(self.inner.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.runtime.block_on(self.inner.flush())
    }
}

#[cfg(target_os = "linux")]
fn socket_creation_flags() -> rustix::net::SocketFlags {
    rustix::net::SocketFlags::CLOEXEC | rustix::net::SocketFlags::NONBLOCK
}

#[cfg(all(unix, not(target_os = "linux")))]
fn socket_creation_flags() -> rustix::net::SocketFlags {
    rustix::net::SocketFlags::empty()
}

#[cfg(target_os = "linux")]
fn configure_socket_for_connect<Fd>(_socket: &Fd) -> io::Result<()>
where
    Fd: std::os::fd::AsFd,
{
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn configure_socket_for_connect<Fd>(socket: &Fd) -> io::Result<()>
where
    Fd: std::os::fd::AsFd,
{
    let raw_fd = socket.as_fd().as_raw_fd();
    set_fd_flag(raw_fd, libc::FD_CLOEXEC)?;
    set_status_flag(raw_fd, libc::O_NONBLOCK)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_fd_flag(raw_fd: libc::c_int, flag: libc::c_int) -> io::Result<()> {
    let flags = unsafe {
        // SAFETY: `fcntl` reads descriptor flags from a valid socket fd.
        libc::fcntl(raw_fd, libc::F_GETFD)
    };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }

    let result = unsafe {
        // SAFETY: `fcntl` updates only descriptor flags for the same valid fd.
        libc::fcntl(raw_fd, libc::F_SETFD, flags | flag)
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_status_flag(raw_fd: libc::c_int, flag: libc::c_int) -> io::Result<()> {
    let flags = unsafe {
        // SAFETY: `fcntl` reads file status flags from a valid socket fd.
        libc::fcntl(raw_fd, libc::F_GETFL)
    };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }

    let result = unsafe {
        // SAFETY: `fcntl` updates only file status flags for the same valid fd.
        libc::fcntl(raw_fd, libc::F_SETFL, flags | flag)
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectProgress {
    Pending,
    Ready,
}

#[cfg(unix)]
fn wait_for_connect_completion<P>(
    socket_path: &Path,
    timeout: Duration,
    mut wait_for_ready: P,
) -> io::Result<()>
where
    P: FnMut(Duration) -> io::Result<ConnectProgress>,
{
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out after {}s connecting to '{}'",
                    timeout.as_secs_f32(),
                    socket_path.display()
                ),
            ));
        }

        if wait_for_ready(remaining)? == ConnectProgress::Ready {
            return Ok(());
        }
    }
}
