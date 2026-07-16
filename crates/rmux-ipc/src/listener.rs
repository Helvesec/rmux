//! Local listener handles.

#[cfg(windows)]
use std::collections::VecDeque;
#[cfg(windows)]
use std::ffi::OsString;
use std::io;
#[cfg(windows)]
use std::mem::size_of;
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, IntoRawFd};

#[cfg(windows)]
use crate::endpoint::current_integrity_label;
#[cfg(windows)]
use crate::is_peer_disconnect;
use crate::{LocalEndpoint, LocalStream, PeerIdentity};
#[cfg(windows)]
use rmux_os::identity::{IdentityResolver, UserIdentity};

#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
#[cfg(windows)]
use windows_sys::Win32::Foundation::LocalFree;
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

/// Local IPC listener.
#[cfg(unix)]
#[derive(Debug)]
pub struct LocalListener {
    inner: tokio::net::UnixListener,
}

/// Local IPC listener backed by a Windows named pipe.
#[cfg(windows)]
#[derive(Debug)]
pub struct LocalListener {
    pipe_name: OsString,
    pending: tokio::sync::Mutex<VecDeque<NamedPipeServer>>,
}

#[cfg(windows)]
const NAMED_PIPE_PENDING_INSTANCES: usize = 4;

impl LocalListener {
    /// Binds a local listener.
    pub fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
        bind_impl(endpoint)
    }

    /// Accepts one local client and returns its byte stream plus peer identity.
    pub async fn accept(&self) -> io::Result<(LocalStream, PeerIdentity)> {
        accept_impl(self).await
    }
}

#[cfg(unix)]
fn bind_impl(endpoint: &LocalEndpoint) -> io::Result<LocalListener> {
    if !endpoint.is_filesystem_path() {
        return bind_rustix_listener(endpoint);
    }
    Ok(LocalListener {
        inner: tokio::net::UnixListener::bind(endpoint.as_path())?,
    })
}

#[cfg(target_os = "linux")]
fn bind_rustix_listener(endpoint: &LocalEndpoint) -> io::Result<LocalListener> {
    use rustix::net::{bind, listen, socket_with, AddressFamily, SocketFlags, SocketType};

    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )?;
    let address = endpoint.socket_addr_unix()?;
    bind(&socket, &address)?;
    listen(&socket, 1024)?;

    let listener = unsafe {
        // SAFETY: `socket` is a listening Unix stream socket and ownership is
        // transferred exactly once into the standard listener.
        std::os::unix::net::UnixListener::from_raw_fd(socket.into_raw_fd())
    };
    listener.set_nonblocking(true)?;
    Ok(LocalListener {
        inner: tokio::net::UnixListener::from_std(listener)?,
    })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn bind_rustix_listener(_endpoint: &LocalEndpoint) -> io::Result<LocalListener> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "abstract Unix socket endpoints are unsupported on this platform",
    ))
}

#[cfg(windows)]
fn bind_impl(endpoint: &LocalEndpoint) -> io::Result<LocalListener> {
    let pipe_name = endpoint.as_pipe_name().to_owned();
    let pending = create_pending_servers(&pipe_name)?;
    Ok(LocalListener {
        pipe_name,
        pending: tokio::sync::Mutex::new(pending),
    })
}

#[cfg(unix)]
async fn accept_impl(listener: &LocalListener) -> io::Result<(LocalStream, PeerIdentity)> {
    let (stream, _addr) = listener.inner.accept().await?;
    let peer = PeerIdentity::from_unix_stream(&stream)?;
    Ok((stream, peer))
}

#[cfg(windows)]
async fn accept_impl(listener: &LocalListener) -> io::Result<(LocalStream, PeerIdentity)> {
    let server = accept_pending_server(listener).await?;
    let peer = PeerIdentity::from_windows_pipe(&server).await;

    Ok((server, peer?))
}

#[cfg(windows)]
async fn accept_pending_server(listener: &LocalListener) -> io::Result<NamedPipeServer> {
    loop {
        let server = take_pending_server(listener).await?;
        if let Err(error) = replenish_pending_servers(listener).await {
            tracing::warn!(
                pipe = ?listener.pipe_name,
                "failed to replenish named-pipe accept backlog before awaiting a client: {error}"
            );
        }
        match server.connect().await {
            Ok(()) => return Ok(server),
            Err(error) if is_peer_disconnect(&error) => {
                tracing::debug!(
                    pipe = ?listener.pipe_name,
                    "discarding abandoned named-pipe instance before accept: {error}"
                );
                if let Err(error) = replenish_pending_servers(listener).await {
                    tracing::warn!(
                        pipe = ?listener.pipe_name,
                        "failed to replenish abandoned named-pipe accept instance: {error}"
                    );
                }
            }
            Err(error) => {
                if let Err(replenish_error) = replenish_pending_servers(listener).await {
                    tracing::warn!(
                        pipe = ?listener.pipe_name,
                        "failed to replenish named-pipe accept instance after error: {replenish_error}"
                    );
                }
                return Err(error);
            }
        }
    }
}

#[cfg(windows)]
async fn take_pending_server(listener: &LocalListener) -> io::Result<NamedPipeServer> {
    let mut pending = listener.pending.lock().await;
    if pending.is_empty() {
        pending.push_back(create_server(&listener.pipe_name, false)?);
    }
    pending
        .pop_front()
        .ok_or_else(|| io::Error::other("named-pipe backlog was exhausted"))
}

#[cfg(windows)]
fn create_pending_servers(pipe_name: &OsString) -> io::Result<VecDeque<NamedPipeServer>> {
    let mut pending = VecDeque::with_capacity(NAMED_PIPE_PENDING_INSTANCES);
    for index in 0..NAMED_PIPE_PENDING_INSTANCES {
        pending.push_back(create_server(pipe_name, index == 0)?);
    }
    Ok(pending)
}

#[cfg(windows)]
async fn replenish_pending_servers(listener: &LocalListener) -> io::Result<()> {
    let mut pending = listener.pending.lock().await;
    while pending.len() < NAMED_PIPE_PENDING_INSTANCES {
        pending.push_back(create_server(&listener.pipe_name, false)?);
    }
    Ok(())
}

#[cfg(windows)]
fn create_server(pipe_name: &OsString, first_instance: bool) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options.first_pipe_instance(first_instance);
    let mut security = SameUserSecurityAttributes::new()?;
    // SAFETY: SECURITY_ATTRIBUTES points at a live self-owned security descriptor
    // for the duration of CreateNamedPipeW inside Tokio.
    unsafe { options.create_with_security_attributes_raw(pipe_name, security.as_mut_ptr()) }
}

#[cfg(windows)]
struct SameUserSecurityAttributes {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

#[cfg(windows)]
impl SameUserSecurityAttributes {
    fn new() -> io::Result<Self> {
        let sid = match IdentityResolver::current()? {
            UserIdentity::Sid(sid) => sid,
            UserIdentity::Uid(_) => {
                return Err(io::Error::other(
                    "windows identity resolver returned a unix uid",
                ));
            }
        };
        let sddl = wide_null(&same_user_pipe_sddl(
            sid.as_ref(),
            current_integrity_label()?,
        )?);
        let mut descriptor = std::ptr::null_mut();

        // SAFETY: sddl is null-terminated UTF-16 and descriptor is an out pointer
        // owned by the caller on success and released with LocalFree.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.cast(),
                bInheritHandle: 0,
            },
        })
    }

    fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
        (&mut self.attributes as *mut SECURITY_ATTRIBUTES).cast()
    }
}

#[cfg(windows)]
impl Drop for SameUserSecurityAttributes {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: descriptor came from ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe {
                LocalFree(self.descriptor.cast());
            }
        }
    }
}

#[cfg(windows)]
fn same_user_pipe_sddl(sid: &str, integrity_label: &str) -> io::Result<String> {
    let integrity_sid = integrity_sddl_sid(integrity_label)?;
    // Keep the pipe scoped to the current account. SSH/desktop process-token
    // asymmetry is handled by client-side pipe owner validation, not by adding
    // broad restricted-code access to the pipe DACL.
    Ok(format!(
        "O:{sid}G:{sid}D:P(A;;GA;;;{sid})S:(ML;;NW;;;{integrity_sid})"
    ))
}

#[cfg(windows)]
fn integrity_sddl_sid(integrity_label: &str) -> io::Result<&'static str> {
    match integrity_label {
        "untrusted" => Ok("UN"),
        "low" => Ok("LW"),
        "medium" => Ok("ME"),
        "high" => Ok("HI"),
        "system" => Ok("SI"),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported Windows integrity label {other:?}"),
        )),
    }
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::{connect_blocking, endpoint_for_label};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    const _: () = assert!(NAMED_PIPE_PENDING_INSTANCES >= 4);

    #[test]
    fn same_user_pipe_sddl_includes_current_user_dacl_and_integrity_label() {
        let sid = "S-1-5-21-1000";

        assert_eq!(
            same_user_pipe_sddl(sid, "medium").expect("medium integrity sddl"),
            "O:S-1-5-21-1000G:S-1-5-21-1000D:P(A;;GA;;;S-1-5-21-1000)S:(ML;;NW;;;ME)"
        );
        assert_eq!(integrity_sddl_sid("low").expect("low integrity"), "LW");
        assert!(integrity_sddl_sid("unknown").is_err());
    }

    #[tokio::test]
    async fn cancelled_accept_does_not_drain_windows_pipe_backlog() -> io::Result<()> {
        let endpoint = endpoint_for_label(format!("listener-cancel-{}", std::process::id()))?;
        let listener = Arc::new(LocalListener::bind(&endpoint)?);

        for _ in 0..NAMED_PIPE_PENDING_INSTANCES {
            let listener = Arc::clone(&listener);
            let accept_task = tokio::spawn(async move { listener.accept().await });
            tokio::time::sleep(Duration::from_millis(10)).await;
            accept_task.abort();
            let _ = accept_task.await;
        }

        let (release_client, client_release) = mpsc::channel();
        let endpoint_for_client = endpoint.clone();
        let client = tokio::task::spawn_blocking(move || {
            let client = connect_blocking(&endpoint_for_client, Duration::from_secs(2))?;
            let _ = client_release.recv_timeout(Duration::from_secs(2));
            drop(client);
            Ok::<(), io::Error>(())
        });
        let accepted = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "accept timed out"))?;

        let (_server, _peer) = accepted?;
        let _ = release_client.send(());
        client.await.map_err(io::Error::other)??;
        Ok(())
    }

    #[tokio::test]
    async fn named_pipe_backlog_accepts_burst_clients_before_accept_loop_runs() -> io::Result<()> {
        let endpoint = endpoint_for_label(format!("listener-burst-{}", std::process::id()))?;
        let listener = LocalListener::bind(&endpoint)?;
        let client_count = NAMED_PIPE_PENDING_INSTANCES;
        let (connected_tx, connected_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let endpoint_for_clients = endpoint.clone();

        let client_thread = std::thread::spawn(move || {
            let mut clients = Vec::with_capacity(client_count);
            for index in 0..client_count {
                match connect_blocking(&endpoint_for_clients, Duration::from_secs(2)) {
                    Ok(client) => {
                        clients.push(client);
                        let _ = connected_tx.send(Ok(index));
                    }
                    Err(error) => {
                        let _ = connected_tx.send(Err(error));
                        return;
                    }
                }
            }
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
            drop(clients);
        });

        for _ in 0..client_count {
            connected_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(io::Error::other)??;
        }

        for _ in 0..client_count {
            tokio::time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "accept timed out"))??;
        }

        let _ = release_tx.send(());
        client_thread.join().expect("client thread");
        Ok(())
    }
}
