use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};

use rmux_ipc::{LocalEndpoint, LocalListener};
use tracing::debug;

const BOUND_SOCKET_MODE: u32 = 0o600;
const UNSAFE_PERMISSION_MASK: u32 = 0o077;
const SOCKET_DIR_PREFIX: &str = "rmux";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SocketFileIdentity {
    device: u64,
    inode: u64,
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
    bind_prepared_unix_listener(socket_path)
}

pub(crate) fn rebind_unix_listener_at(
    socket_path: &Path,
    current_identity: Option<SocketFileIdentity>,
) -> io::Result<BoundUnixListener> {
    if socket_path.as_os_str().is_empty() {
        return bind_empty_socket_listener();
    }
    prepare_socket_parent(socket_path)?;
    remove_rebindable_socket(socket_path, current_identity)?;
    bind_prepared_unix_listener(socket_path)
}

fn bind_prepared_unix_listener(socket_path: &Path) -> io::Result<BoundUnixListener> {
    let endpoint = LocalEndpoint::from_path(socket_path.to_path_buf());
    let listener = LocalListener::bind(&endpoint)?;
    enforce_bound_socket_permissions(socket_path)?;
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
    let parent = socket_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path '{}' has no parent directory",
                socket_path.display()
            ),
        )
    })?;

    ensure_parent_directory(parent)
}

pub(crate) fn ensure_parent_directory(parent: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
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

fn managed_rmux_socket_directory(path: &Path) -> io::Result<Option<PathBuf>> {
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
    let mode = metadata.permissions().mode();
    if mode & UNSAFE_PERMISSION_MASK != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket directory '{}' must not be accessible by group or others",
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
    Ok(())
}

fn enforce_bound_socket_permissions(socket_path: &Path) -> io::Result<()> {
    validate_bound_socket(socket_path, false)?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(BOUND_SOCKET_MODE))?;
    validate_bound_socket(socket_path, true)
}

fn validate_bound_socket(socket_path: &Path, require_owner_only: bool) -> io::Result<()> {
    let metadata = socket_metadata(socket_path, io::ErrorKind::PermissionDenied)?;
    ensure_directory(socket_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path '{}' has no parent directory",
                socket_path.display()
            ),
        )
    })?)?;
    let user_id = real_user_id()?;
    if metadata.uid() != user_id {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket {} has unsafe ownership", socket_path.display()),
        ));
    }
    if require_owner_only && metadata.permissions().mode() & 0o777 != BOUND_SOCKET_MODE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket {} has unsafe permissions", socket_path.display()),
        ));
    }
    Ok(())
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

    match StdUnixStream::connect(socket_path) {
        Ok(_stream) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("socket '{}' is already in use", socket_path.display()),
        )),
        Err(error) if indicates_stale_socket(&error) => {
            debug!(
                "removing stale socket '{}' after failed connect probe: {error}",
                socket_path.display()
            );
            match fs::remove_file(socket_path) {
                Ok(()) => Ok(()),
                Err(remove_error) if remove_error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(remove_error) => Err(remove_error),
            }
        }
        Err(error) => Err(error),
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

        let error = match rebind_unix_listener_at(&socket_path, None) {
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
            rebind_unix_listener_at(&socket_path, bound.identity).expect("rebind current socket");

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

    fn cleanup_socket_dir(socket_path: &Path) {
        let _ = fs::remove_file(socket_path);
        if let Some(parent) = socket_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}
