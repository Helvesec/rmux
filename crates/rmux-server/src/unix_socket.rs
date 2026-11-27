use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rmux_ipc::{LocalEndpoint, LocalListener, SocketPresence};
use tracing::debug;

pub(crate) const OWNER_ONLY_DIRECTORY_MODE: u32 = 0o700;
pub(crate) const SHARED_DIRECTORY_MODE: u32 = 0o711;
pub(crate) const OWNER_ONLY_SOCKET_MODE: u32 = 0o600;
pub(crate) const SHARED_SOCKET_MODE: u32 = 0o666;
const SOCKET_DIR_PREFIX: &str = "rmux";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnixTransportAccess {
    OwnerOnly,
    AllowListed,
}

impl UnixTransportAccess {
    #[must_use]
    pub(crate) const fn socket_mode(self) -> u32 {
        match self {
            Self::OwnerOnly => OWNER_ONLY_SOCKET_MODE,
            Self::AllowListed => SHARED_SOCKET_MODE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SocketFileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

pub(crate) struct BoundUnixListener {
    pub(crate) listener: LocalListener,
    pub(crate) identity: Option<SocketFileIdentity>,
}

pub(crate) fn bind_unix_listener_at(socket_path: &Path) -> io::Result<BoundUnixListener> {
    if socket_path.as_os_str().is_empty() {
        return bind_empty_socket_listener();
    }
    prepare_socket_path(socket_path)?;
    bind_prepared_unix_listener(socket_path, UnixTransportAccess::OwnerOnly)
}

pub(crate) fn rebind_unix_listener_at(
    socket_path: &Path,
    current_identity: Option<SocketFileIdentity>,
    access: UnixTransportAccess,
) -> io::Result<BoundUnixListener> {
    if socket_path.as_os_str().is_empty() {
        return bind_empty_socket_listener();
    }
    prepare_socket_parent(socket_path)?;
    remove_rebindable_socket(socket_path, current_identity)?;
    bind_prepared_unix_listener(socket_path, access)
}

fn bind_prepared_unix_listener(
    socket_path: &Path,
    access: UnixTransportAccess,
) -> io::Result<BoundUnixListener> {
    let endpoint = LocalEndpoint::from_path(socket_path.to_path_buf());
    let listener = LocalListener::bind(&endpoint)?;
    enforce_bound_socket_permissions(socket_path, access.socket_mode())?;
    let identity = socket_file_identity(socket_path)?;
    Ok(BoundUnixListener {
        listener,
        identity: Some(identity),
    })
}

fn bind_empty_socket_listener() -> io::Result<BoundUnixListener> {
    let endpoint = rmux_ipc::resolve_endpoint(None, Some(Path::new("")))?;
    let listener = LocalListener::bind(&endpoint)?;
    Ok(BoundUnixListener {
        listener,
        identity: None,
    })
}

fn prepare_socket_path(socket_path: &Path) -> io::Result<()> {
    prepare_socket_parent(socket_path)?;
    remove_stale_socket_if_needed(socket_path)
}

fn prepare_socket_parent(socket_path: &Path) -> io::Result<()> {
    ensure_parent_directory(socket_parent_or_current(socket_path)?)
}

pub(crate) fn ensure_parent_directory(parent: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(OWNER_ONLY_DIRECTORY_MODE);
    match builder.create(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::metadata(parent)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("socket parent '{}' is not a directory", parent.display()),
                ));
            }
        }
        Err(error) => return Err(error),
    }

    if let Some(managed_directory) = managed_rmux_socket_directory(parent)? {
        ensure_safe_rmux_socket_directory(&managed_directory)?;
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket directory '{}' is not a directory", path.display()),
        ));
    }
    Ok(())
}

pub(crate) fn managed_rmux_socket_directory(path: &Path) -> io::Result<Option<PathBuf>> {
    let expected = format!("{SOCKET_DIR_PREFIX}-{}", real_user_id()?);
    for ancestor in path.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) == Some(expected.as_str()) {
            return Ok(Some(ancestor.to_path_buf()));
        }
    }
    Ok(None)
}

fn ensure_safe_rmux_socket_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket directory '{}' is not a plain directory",
                path.display()
            ),
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != OWNER_ONLY_DIRECTORY_MODE && mode != SHARED_DIRECTORY_MODE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket directory '{}' has unsafe permissions",
                path.display()
            ),
        ));
    }
    let user_id = real_user_id()?;
    if metadata.uid() != user_id {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket directory '{}' has unsafe ownership", path.display()),
        ));
    }
    // Every `-L` server of this user shares this directory. Its traverse bits
    // change only through the access controller, under the managed lock that
    // serializes every sibling's narrowing scan; a bind never touches them.
    Ok(())
}

fn enforce_bound_socket_permissions(socket_path: &Path, expected_mode: u32) -> io::Result<()> {
    validate_bound_socket(socket_path, None)?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(expected_mode))?;
    validate_bound_socket(socket_path, Some(expected_mode))
}

fn validate_bound_socket(socket_path: &Path, expected_mode: Option<u32>) -> io::Result<()> {
    let metadata = socket_metadata(socket_path, io::ErrorKind::PermissionDenied)?;
    ensure_directory(socket_parent_or_current(socket_path)?)?;
    let user_id = real_user_id()?;
    if metadata.uid() != user_id {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket {} has unsafe ownership", socket_path.display()),
        ));
    }
    if expected_mode.is_some_and(|mode| metadata.permissions().mode() & 0o777 != mode) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket {} has unsafe permissions", socket_path.display()),
        ));
    }
    Ok(())
}

fn socket_parent_or_current(socket_path: &Path) -> io::Result<&Path> {
    let Some(parent) = rmux_os::path::parent_or_current(socket_path) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path '{}' has no parent directory",
                socket_path.display()
            ),
        ));
    };
    Ok(parent)
}

pub(crate) fn socket_file_identity(socket_path: &Path) -> io::Result<SocketFileIdentity> {
    let metadata = socket_metadata(socket_path, io::ErrorKind::PermissionDenied)?;
    ensure_socket_owner(&metadata, socket_path)?;
    Ok(identity_from_metadata(&metadata))
}

pub(crate) fn remove_stale_socket_if_needed(socket_path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "socket path '{}' exists but is not a Unix socket",
                socket_path.display()
            ),
        ));
    }

    // A stream connect cannot prove the socket stale: Darwin refuses it when a
    // live listener's accept queue is full, and Linux blocks on that queue.
    let endpoint = LocalEndpoint::from_path(socket_path.to_path_buf());
    match rmux_ipc::probe_socket_presence(&endpoint)? {
        SocketPresence::Bound => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("socket '{}' is already in use", socket_path.display()),
        )),
        SocketPresence::Unbound => {
            debug!(
                "removing stale socket '{}': no live socket is bound to it",
                socket_path.display()
            );
            remove_file_if_present(socket_path)
        }
        SocketPresence::Missing => Ok(()),
    }
}

pub(crate) fn remove_socket_file_if_identity_matches(
    socket_path: &Path,
    expected_identity: SocketFileIdentity,
) -> io::Result<bool> {
    let metadata = match owned_socket_metadata(socket_path, io::ErrorKind::PermissionDenied) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };

    if identity_from_metadata(&metadata) != expected_identity {
        return Ok(false);
    }

    match fs::remove_file(socket_path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_rebindable_socket(
    socket_path: &Path,
    current_identity: Option<SocketFileIdentity>,
) -> io::Result<()> {
    let metadata = match owned_socket_metadata(socket_path, io::ErrorKind::AlreadyExists) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if current_identity.is_some_and(|identity| identity == identity_from_metadata(&metadata)) {
        return remove_file_if_present(socket_path);
    }

    remove_stale_socket_if_needed(socket_path)
}

fn owned_socket_metadata(
    socket_path: &Path,
    wrong_type_kind: io::ErrorKind,
) -> io::Result<fs::Metadata> {
    let metadata = socket_metadata(socket_path, wrong_type_kind)?;
    ensure_socket_owner(&metadata, socket_path)?;
    Ok(metadata)
}

fn socket_metadata(socket_path: &Path, wrong_type_kind: io::ErrorKind) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(socket_path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            wrong_type_kind,
            format!(
                "socket path '{}' is not a plain Unix socket",
                socket_path.display()
            ),
        ));
    }
    Ok(metadata)
}

fn ensure_socket_owner(metadata: &fs::Metadata, socket_path: &Path) -> io::Result<()> {
    let user_id = real_user_id()?;
    if metadata.uid() != user_id {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket {} has unsafe ownership", socket_path.display()),
        ));
    }
    Ok(())
}

fn identity_from_metadata(metadata: &fs::Metadata) -> SocketFileIdentity {
    SocketFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn remove_file_if_present(socket_path: &Path) -> io::Result<()> {
    match fs::remove_file(socket_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(crate) fn indicates_stale_socket(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

pub(crate) fn real_user_id() -> io::Result<u32> {
    Ok(rmux_os::identity::real_user_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn sigusr1_rebind_refuses_a_live_foreign_socket() {
        let socket_path = unique_socket_path("live-foreign-rebind");
        let parent = socket_path.parent().expect("socket parent");
        fs::create_dir_all(parent).expect("create socket parent");
        let foreign = StdUnixListener::bind(&socket_path).expect("bind foreign socket");

        let error =
            match rebind_unix_listener_at(&socket_path, None, UnixTransportAccess::OwnerOnly) {
                Ok(_) => panic!("live foreign socket must not be unlinked"),
                Err(error) => error,
            };

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(
            UnixStream::connect(&socket_path).is_ok(),
            "foreign socket must remain connectable"
        );
        drop(foreign);
        cleanup_socket_dir(&socket_path);
    }

    #[tokio::test]
    async fn socket_cleanup_identity_does_not_remove_recreated_foreign_socket() {
        let socket_path = unique_socket_path("foreign-cleanup");
        let bound = bind_unix_listener_at(&socket_path).expect("bind first socket");
        remove_file_if_present(&socket_path).expect("unlink first socket path");
        let foreign = StdUnixListener::bind(&socket_path).expect("bind foreign replacement");

        let removed = remove_socket_file_if_identity_matches(
            &socket_path,
            bound.identity.expect("filesystem socket identity"),
        )
        .expect("identity guarded cleanup");

        assert!(!removed, "cleanup must not remove a different socket inode");
        assert!(
            UnixStream::connect(&socket_path).is_ok(),
            "foreign socket must remain connectable"
        );
        drop(foreign);
        drop(bound.listener);
        cleanup_socket_dir(&socket_path);
    }

    #[tokio::test]
    async fn sigusr1_rebind_can_replace_the_current_socket_identity() {
        let socket_path = unique_socket_path("current-rebind");
        let bound = bind_unix_listener_at(&socket_path).expect("bind first socket");

        let rebound =
            rebind_unix_listener_at(&socket_path, bound.identity, UnixTransportAccess::OwnerOnly)
                .expect("rebind current socket");

        assert!(UnixStream::connect(&socket_path).is_ok());
        drop(rebound.listener);
        drop(bound.listener);
        cleanup_socket_dir(&socket_path);
    }

    fn unique_socket_path(label: &str) -> PathBuf {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        PathBuf::from(format!(
            "/tmp/rmx{}{}{}",
            std::process::id(),
            label.as_bytes()[0],
            unique_id
        ))
        .join("s")
    }

    #[test]
    fn daemon_bind_keeps_the_socket_of_a_saturated_live_listener() {
        assert_saturated_listener_survives("saturated-bind", |socket_path| {
            bind_unix_listener_at(socket_path).map(drop)
        });
    }

    #[test]
    fn sigusr1_rebind_keeps_the_socket_of_a_saturated_live_listener() {
        assert_saturated_listener_survives("saturated-rebind", |socket_path| {
            rebind_unix_listener_at(socket_path, None, UnixTransportAccess::OwnerOnly).map(drop)
        });
    }

    /// Binds beside a live listener whose accept queue is full: Darwin refuses
    /// a stream connect to it and Linux blocks one, yet it is not stale.
    fn assert_saturated_listener_survives(label: &str, bind: fn(&Path) -> io::Result<()>) {
        let socket_path = unique_socket_path(label);
        let parent = socket_path.parent().expect("socket parent");
        fs::create_dir_all(parent).expect("create socket parent");
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).expect("secure parent");
        let live = StdUnixListener::bind(&socket_path).expect("bind live listener");
        rustix::net::listen(&live, 1).expect("shrink backlog");
        let queued = fill_accept_queue(&socket_path);
        let inode = fs::symlink_metadata(&socket_path)
            .expect("live socket")
            .ino();

        let (sender, receiver) = std::sync::mpsc::channel();
        let path_for_bind = socket_path.clone();
        std::thread::spawn(move || {
            // Binding the listener itself registers it with a Tokio reactor.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let _context = runtime.enter();
            let _ = sender.send(bind(&path_for_bind));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("binding blocked on the saturated listener's queue");

        let inode_after = fs::symlink_metadata(&socket_path).ok().map(|m| m.ino());
        assert_eq!(
            inode_after,
            Some(inode),
            "the live listener's socket was unlinked or replaced"
        );
        assert_eq!(
            result.expect_err("a live listener holds the path").kind(),
            io::ErrorKind::AddrInUse
        );
        drop((queued, live));
        cleanup_socket_dir(&socket_path);
    }

    /// Fills the queue until Darwin refuses (ECONNREFUSED) or Linux rejects
    /// (EAGAIN) a non-blocking connect.
    fn fill_accept_queue(socket_path: &Path) -> Vec<std::os::fd::OwnedFd> {
        use rustix::net::{connect, socket, AddressFamily, SocketAddrUnix, SocketType};

        let address = SocketAddrUnix::new(socket_path).expect("socket address");
        let mut queued = Vec::new();
        loop {
            let client = socket(AddressFamily::UNIX, SocketType::STREAM, None).expect("socket");
            rustix::io::ioctl_fionbio(&client, true).expect("non-blocking client");
            match connect(&client, &address) {
                Ok(()) => queued.push(client),
                Err(rustix::io::Errno::CONNREFUSED | rustix::io::Errno::AGAIN) => return queued,
                Err(error) => panic!("unexpected connect error: {error}"),
            }
            assert!(queued.len() < 64, "the accept queue never filled");
        }
    }

    fn cleanup_socket_dir(socket_path: &Path) {
        let _ = fs::remove_file(socket_path);
        if let Some(parent) = socket_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}
