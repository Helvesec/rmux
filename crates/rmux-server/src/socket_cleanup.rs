use std::path::PathBuf;

#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use crate::unix_socket::SocketFileIdentity;

#[cfg(unix)]
pub(crate) struct SocketCleanup {
    socket_path: PathBuf,
    socket_identity: Option<SocketFileIdentity>,
}

#[cfg(unix)]
impl SocketCleanup {
    pub(crate) fn new(socket_path: PathBuf, socket_identity: Option<SocketFileIdentity>) -> Self {
        let socket_identity =
            socket_identity.or_else(|| crate::unix_socket::socket_file_identity(&socket_path).ok());
        Self {
            socket_path,
            socket_identity,
        }
    }

    pub(crate) fn socket_identity(&self) -> Option<SocketFileIdentity> {
        self.socket_identity
    }

    pub(crate) fn update_socket_identity(&mut self, socket_identity: Option<SocketFileIdentity>) {
        self.socket_identity = socket_identity;
    }

    pub(crate) fn cleanup_now(&mut self) {
        cleanup_socket_artifacts(&self.socket_path, self.socket_identity.take());
    }
}

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        cleanup_socket_artifacts(&self.socket_path, self.socket_identity.take());
    }
}

#[cfg(windows)]
pub(crate) struct SocketCleanup;

#[cfg(windows)]
impl SocketCleanup {
    pub(crate) fn new(_socket_path: PathBuf) -> Self {
        Self
    }

    pub(crate) fn cleanup_now(&mut self) {}
}

#[cfg(unix)]
fn cleanup_socket_artifacts(socket_path: &Path, socket_identity: Option<SocketFileIdentity>) {
    if let Some(socket_identity) = socket_identity {
        let _ = crate::unix_socket::remove_socket_file_if_identity_matches(
            socket_path,
            socket_identity,
        );
    }
    for lock_path in startup_lock_paths(socket_path) {
        let _ = remove_regular_file_if_present(&lock_path);
    }
    crate::tmux_shim::cleanup_tmux_shim(socket_path);
}

#[cfg(unix)]
fn startup_lock_paths(socket_path: &Path) -> Vec<PathBuf> {
    let Some(parent) = socket_path.parent() else {
        return Vec::new();
    };
    let Some(file_name) = socket_path.file_name() else {
        return Vec::new();
    };

    let mut startup_lock_name = file_name.to_os_string();
    startup_lock_name.push(".startup-lock");
    let mut legacy_lock_name = file_name.to_os_string();
    legacy_lock_name.push(".lock");

    vec![
        parent.join(startup_lock_name),
        parent.join(legacy_lock_name),
    ]
}

#[cfg(unix)]
fn remove_regular_file_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

    #[tokio::test]
    async fn drop_preserves_recreated_foreign_socket() {
        let socket_path = unique_socket_path();
        let bound = crate::unix_socket::bind_unix_listener_at(&socket_path).expect("bind socket");
        let cleanup = SocketCleanup::new(socket_path.clone(), bound.identity);
        std::fs::remove_file(&socket_path).expect("unlink original socket path");
        let foreign = StdUnixListener::bind(&socket_path).expect("bind foreign replacement");

        drop(cleanup);

        assert!(
            UnixStream::connect(&socket_path).is_ok(),
            "cleanup must not remove a different socket inode"
        );
        drop(foreign);
        drop(bound.listener);
        cleanup_socket_dir(&socket_path);
    }

    fn unique_socket_path() -> PathBuf {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!("/tmp/rmxcl{}{}", std::process::id(), unique_id)).join("s")
    }

    fn cleanup_socket_dir(socket_path: &Path) {
        let _ = std::fs::remove_file(socket_path);
        if let Some(parent) = socket_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
