//! Local listener handles.

#[cfg(windows)]
use std::collections::VecDeque;
#[cfg(windows)]
use std::ffi::OsString;
use std::io;
#[cfg(windows)]
use std::mem::size_of;

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
const NAMED_PIPE_PENDING_INSTANCES: usize = 1;

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
    Ok(LocalListener {
        inner: tokio::net::UnixListener::bind(endpoint.as_path())?,
    })
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
    if let Err(error) = replenish_pending_server(listener).await {
        tracing::warn!(
            pipe = ?listener.pipe_name,
            "failed to replenish named-pipe accept backlog after accepting a client: {error}"
        );
    }

    Ok((server, peer?))
}

#[cfg(windows)]
async fn accept_pending_server(listener: &LocalListener) -> io::Result<NamedPipeServer> {
    loop {
        let server = take_pending_server(listener).await?;
        match server.connect().await {
            Ok(()) => return Ok(server),
            Err(error) if is_peer_disconnect(&error) => {
                tracing::debug!(
                    pipe = ?listener.pipe_name,
                    "discarding abandoned named-pipe instance before accept: {error}"
                );
                if let Err(error) = replenish_pending_server(listener).await {
                    tracing::warn!(
                        pipe = ?listener.pipe_name,
                        "failed to replenish abandoned named-pipe accept instance: {error}"
                    );
                }
            }
            Err(error) => {
                if let Err(replenish_error) = replenish_pending_server(listener).await {
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
async fn replenish_pending_server(listener: &LocalListener) -> io::Result<()> {
    let next = create_server(&listener.pipe_name, false)?;
    let mut pending = listener.pending.lock().await;
    pending.push_back(next);
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
        let sddl = wide_null(&format!("O:{sid}G:{sid}D:P(A;;GA;;;{sid})"));
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
}
