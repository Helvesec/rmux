use std::path::PathBuf;

#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use crate::unix_socket::SocketFileIdentity;

#[cfg(unix)]
pub(crate) struct SocketCleanup {
    socket_path: PathBuf,
    socket_identity: Option<SocketFileIdentity>,
    armed: bool,
}

#[cfg(unix)]
impl SocketCleanup {
    pub(crate) fn new(socket_path: PathBuf, socket_identity: Option<SocketFileIdentity>) -> Self {
        let socket_identity =
            socket_identity.or_else(|| crate::unix_socket::socket_file_identity(&socket_path).ok());
        Self {
            socket_path,
            socket_identity,
            armed: true,
        }
    }

    pub(crate) fn socket_identity(&self) -> Option<SocketFileIdentity> {
        self.socket_identity
    }

    pub(crate) fn update_socket_identity(&mut self, socket_identity: Option<SocketFileIdentity>) {
        self.socket_identity = socket_identity;
        self.armed = true;
    }

    pub(crate) fn cleanup_now(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        cleanup_socket_artifacts(&self.socket_path, self.socket_identity.take());
    }
}

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        self.cleanup_now();
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
    if !release_socket_generation(socket_path, socket_identity) {
        return;
    }
    for lock_path in startup_lock_paths(socket_path) {
        let _ = remove_regular_file_if_present(&lock_path);
    }
    crate::tmux_shim::cleanup_tmux_shim(socket_path);
}

#[cfg(unix)]
fn release_socket_generation(
    socket_path: &Path,
    socket_identity: Option<SocketFileIdentity>,
) -> bool {
    let Some(socket_identity) = socket_identity else {
        return socket_path.as_os_str().is_empty() || socket_path_is_absent(socket_path);
    };

    match crate::unix_socket::remove_socket_file_if_identity_matches(socket_path, socket_identity) {
        Ok(true) => true,
        Ok(false) => socket_path_is_absent(socket_path),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn socket_path_is_absent(socket_path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(socket_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
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
        let _env_lock = crate::test_env::lock_async().await;
        let socket_path = unique_socket_path();
        let bound = crate::unix_socket::bind_unix_listener_at(&socket_path).expect("bind socket");
        let cleanup = SocketCleanup::new(socket_path.clone(), bound.identity);
        std::fs::remove_file(&socket_path).expect("unlink original socket path");
        let foreign = StdUnixListener::bind(&socket_path).expect("bind foreign replacement");
        let lock_paths = startup_lock_paths(&socket_path);
        for lock_path in &lock_paths {
            std::fs::write(lock_path, b"replacement").expect("write replacement lock");
        }
        let shim_path = crate::tmux_shim::test_shim_path(&socket_path);
        std::fs::create_dir_all(shim_path.parent().expect("shim parent"))
            .expect("create replacement shim directory");
        std::fs::write(&shim_path, b"replacement").expect("write replacement shim");

        drop(cleanup);

        assert!(
            UnixStream::connect(&socket_path).is_ok(),
            "cleanup must not remove a different socket inode"
        );
        for lock_path in &lock_paths {
            assert!(
                lock_path.is_file(),
                "cleanup must preserve replacement startup locks"
            );
        }
        assert!(
            shim_path.is_file(),
            "cleanup must preserve the replacement tmux shim"
        );
        drop(foreign);
        for lock_path in lock_paths {
            let _ = std::fs::remove_file(lock_path);
        }
        crate::tmux_shim::cleanup_tmux_shim(&socket_path);
        drop(bound.listener);
        cleanup_socket_dir(&socket_path);
    }

    #[tokio::test]
    async fn cleanup_now_disarms_drop_for_recreated_generation_artifacts() {
        let _env_lock = crate::test_env::lock_async().await;
        let socket_path = unique_socket_path();
        let bound = crate::unix_socket::bind_unix_listener_at(&socket_path).expect("bind socket");
        let mut cleanup = SocketCleanup::new(socket_path.clone(), bound.identity);

        cleanup.cleanup_now();
        assert!(
            !socket_path.exists(),
            "the original socket should be removed by explicit cleanup"
        );

        let lock_paths = startup_lock_paths(&socket_path);
        for lock_path in &lock_paths {
            std::fs::write(lock_path, b"replacement").expect("write replacement lock");
        }
        let shim_path = crate::tmux_shim::test_shim_path(&socket_path);
        std::fs::create_dir_all(shim_path.parent().expect("shim parent"))
            .expect("create replacement shim directory");
        std::fs::write(&shim_path, b"replacement").expect("write replacement shim");

        drop(cleanup);

        assert!(
            !socket_path.exists(),
            "drop must not recreate or reclean the released socket path"
        );
        for lock_path in &lock_paths {
            assert!(
                lock_path.is_file(),
                "drop must preserve replacement startup locks"
            );
        }
        assert!(
            shim_path.is_file(),
            "drop must preserve the replacement tmux shim"
        );

        for lock_path in lock_paths {
            let _ = std::fs::remove_file(lock_path);
        }
        crate::tmux_shim::cleanup_tmux_shim(&socket_path);
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
