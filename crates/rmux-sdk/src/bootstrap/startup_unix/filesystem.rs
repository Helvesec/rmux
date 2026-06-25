use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::identity::{FileIdentity, ParentAnchor};
use super::{StartupError, SOCKET_DIRECTORY_MODE, UNSAFE_PERMISSION_MASK};

const STALE_PROBE_TIMEOUT: Duration = Duration::from_millis(50);
const CUSTOM_SOCKET_PARENT_OTHER_WRITABLE_MASK: u32 = 0o002;
const CUSTOM_SOCKET_PARENT_GROUP_WRITABLE_MASK: u32 = 0o020;

#[derive(Debug)]
pub(super) struct PreparedSocketParent {
    pub(super) lock_path: PathBuf,
    pub(super) parent_anchor: Option<ParentAnchor>,
}

pub(super) fn reject_socket_symlink(
    socket_path: &Path,
    owner_uid: u32,
) -> Result<(), StartupError> {
    reject_symlinked_existing_components(socket_path, owner_uid)
}

pub(super) fn startup_lock_path(socket_path: &Path) -> PathBuf {
    let mut lock_name = socket_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    lock_name.push(".startup-lock");
    let parent = socket_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    parent.join(lock_name)
}

pub(super) fn prepare_socket_parent(
    socket_path: &Path,
    parent: &Path,
    owner_uid: u32,
) -> Result<PreparedSocketParent, StartupError> {
    reject_symlinked_existing_components(parent, owner_uid)?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            prepare_symlinked_socket_parent(socket_path, parent, owner_uid)
        }
        Ok(metadata) if is_shared_sticky_directory(&metadata, owner_uid) => {
            let lock_dir = shared_startup_lock_dir(parent, owner_uid);
            ensure_startup_lock_dir(&lock_dir, owner_uid)?;
            Ok(PreparedSocketParent {
                lock_path: startup_lock_path_in_dir(socket_path, &lock_dir),
                parent_anchor: Some(ParentAnchor::open(parent)?),
            })
        }
        Ok(metadata) => {
            let parent_anchor = Some(ParentAnchor::open(parent)?);
            let lock_path = if is_default_owner_socket_directory(parent, owner_uid) {
                validate_directory_metadata(parent, &metadata, owner_uid)?;
                startup_lock_path(socket_path)
            } else {
                validate_existing_socket_parent(parent, &metadata, owner_uid)?;
                if custom_parent_needs_private_lock(&metadata) {
                    private_startup_lock_path(socket_path, owner_uid)?
                } else {
                    startup_lock_path(socket_path)
                }
            };
            Ok(PreparedSocketParent {
                lock_path,
                parent_anchor,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_owner_only_directory(parent, owner_uid)?;
            Ok(PreparedSocketParent {
                lock_path: startup_lock_path(socket_path),
                parent_anchor: Some(ParentAnchor::open(parent)?),
            })
        }
        Err(error) => Err(StartupError::Filesystem {
            operation: "stat socket parent directory",
            path: parent.to_path_buf(),
            source: error,
        }),
    }
}

fn prepare_symlinked_socket_parent(
    socket_path: &Path,
    parent: &Path,
    owner_uid: u32,
) -> Result<PreparedSocketParent, StartupError> {
    let metadata = fs::metadata(parent).map_err(|error| StartupError::Filesystem {
        operation: "stat resolved socket parent directory",
        path: parent.to_path_buf(),
        source: error,
    })?;

    if !is_shared_sticky_directory(&metadata, owner_uid) {
        return Err(StartupError::SymlinkRejected {
            path: parent.to_path_buf(),
        });
    }

    let lock_dir = shared_startup_lock_dir(parent, owner_uid);
    ensure_startup_lock_dir(&lock_dir, owner_uid)?;
    Ok(PreparedSocketParent {
        lock_path: startup_lock_path_in_dir(socket_path, &lock_dir),
        parent_anchor: None,
    })
}

fn is_shared_sticky_directory(metadata: &fs::Metadata, owner_uid: u32) -> bool {
    metadata.file_type().is_dir()
        && metadata.uid() != owner_uid
        && has_mode_bit(metadata.mode(), libc::S_ISVTX)
        && metadata.mode() & 0o022 != 0
}

pub(super) fn has_mode_bit<T>(mode: u32, bit: T) -> bool
where
    T: Into<u32>,
{
    mode & bit.into() != 0
}

fn shared_startup_lock_dir(parent: &Path, owner_uid: u32) -> PathBuf {
    parent
        .join(format!("rmux-{owner_uid}"))
        .join("startup-locks")
}

fn private_startup_lock_path(socket_path: &Path, owner_uid: u32) -> Result<PathBuf, StartupError> {
    let root = private_startup_lock_root()?;
    let lock_dir = shared_startup_lock_dir(&root, owner_uid);
    ensure_startup_lock_dir(&lock_dir, owner_uid)?;
    Ok(startup_lock_path_in_dir(socket_path, &lock_dir))
}

fn private_startup_lock_root() -> Result<PathBuf, StartupError> {
    let fallback = PathBuf::from("/tmp");
    fs::canonicalize(&fallback).map_err(|error| StartupError::Filesystem {
        operation: "resolve private startup lock root",
        path: fallback,
        source: error,
    })
}

fn ensure_startup_lock_dir(lock_dir: &Path, owner_uid: u32) -> Result<(), StartupError> {
    if let Some(base_dir) = lock_dir.parent() {
        ensure_owner_only_directory(base_dir, owner_uid)?;
    }
    ensure_owner_only_directory(lock_dir, owner_uid)
}

fn is_default_owner_socket_directory(parent: &Path, owner_uid: u32) -> bool {
    parent
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == format!("rmux-{owner_uid}"))
}

fn startup_lock_path_in_dir(socket_path: &Path, lock_dir: &Path) -> PathBuf {
    let mut lock_name = OsString::new();
    if let Some(file_name) = socket_path.file_name() {
        lock_name.push(file_name);
    } else {
        lock_name.push("socket");
    }
    lock_name.push(format!(
        ".{:016x}.startup-lock",
        stable_path_hash(socket_path)
    ));
    lock_dir.join(lock_name)
}

fn stable_path_hash(path: &Path) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in path.as_os_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn reject_symlinked_existing_components(path: &Path, owner_uid: u32) -> Result<(), StartupError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }

        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let targets_shared_sticky =
                    symlink_targets_shared_sticky_directory(&current, owner_uid)?;
                let trusted_platform_alias = trusted_platform_directory_alias(&current)?;
                if !trusted_platform_alias && !targets_shared_sticky {
                    return Err(StartupError::SymlinkRejected { path: current });
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(StartupError::Filesystem {
                    operation: "stat socket path component for symlink check",
                    path: current,
                    source: error,
                });
            }
        }
    }
    Ok(())
}

fn symlink_targets_shared_sticky_directory(
    path: &Path,
    owner_uid: u32,
) -> Result<bool, StartupError> {
    let metadata = fs::metadata(path).map_err(|error| StartupError::Filesystem {
        operation: "stat symlink target for socket path component",
        path: path.to_path_buf(),
        source: error,
    })?;
    Ok(is_shared_sticky_directory(&metadata, owner_uid))
}

fn trusted_platform_directory_alias(path: &Path) -> Result<bool, StartupError> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }

    let Some(expected_target) = macos_system_alias_target(path) else {
        return Ok(false);
    };
    let resolved = fs::canonicalize(path).map_err(|error| StartupError::Filesystem {
        operation: "resolve trusted platform directory alias",
        path: path.to_path_buf(),
        source: error,
    })?;
    let expected = fs::canonicalize(expected_target).map_err(|error| StartupError::Filesystem {
        operation: "resolve trusted platform directory alias target",
        path: expected_target.to_path_buf(),
        source: error,
    })?;
    Ok(resolved == expected)
}

fn macos_system_alias_target(path: &Path) -> Option<&'static Path> {
    if path == Path::new("/tmp") {
        Some(Path::new("/private/tmp"))
    } else if path == Path::new("/var") {
        Some(Path::new("/private/var"))
    } else {
        None
    }
}

pub(super) fn ensure_owner_only_directory(path: &Path, owner_uid: u32) -> Result<(), StartupError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory_metadata(path, &metadata, owner_uid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_owner_only_directory(path)?;
            let metadata =
                fs::symlink_metadata(path).map_err(|error| StartupError::Filesystem {
                    operation: "stat owner-only directory after create",
                    path: path.to_path_buf(),
                    source: error,
                })?;
            validate_directory_metadata(path, &metadata, owner_uid)
        }
        Err(error) => Err(StartupError::Filesystem {
            operation: "stat owner-only directory",
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

fn validate_directory_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    owner_uid: u32,
) -> Result<(), StartupError> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: path.to_path_buf(),
        });
    }
    if !file_type.is_dir() {
        return Err(StartupError::Filesystem {
            operation: "ensure owner-only directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "expected a directory at this path",
            ),
        });
    }
    if metadata.uid() != owner_uid {
        return Err(StartupError::UnsafeOwner {
            path: path.to_path_buf(),
            expected_uid: owner_uid,
            actual_uid: metadata.uid(),
        });
    }
    let mode = metadata.mode() & 0o7777;
    if mode != SOCKET_DIRECTORY_MODE {
        let permissions = fs::Permissions::from_mode(SOCKET_DIRECTORY_MODE);
        fs::set_permissions(path, permissions).map_err(|error| StartupError::Filesystem {
            operation: "tighten directory permissions",
            path: path.to_path_buf(),
            source: error,
        })?;
        let metadata = fs::symlink_metadata(path).map_err(|error| StartupError::Filesystem {
            operation: "stat owner-only directory after chmod",
            path: path.to_path_buf(),
            source: error,
        })?;
        let mode = metadata.mode() & 0o7777;
        if mode & UNSAFE_PERMISSION_MASK != 0 {
            return Err(StartupError::UnsafePermissions {
                path: path.to_path_buf(),
                mode,
            });
        }
    }
    Ok(())
}

fn validate_existing_socket_parent(
    path: &Path,
    metadata: &fs::Metadata,
    owner_uid: u32,
) -> Result<(), StartupError> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: path.to_path_buf(),
        });
    }
    if !file_type.is_dir() {
        return Err(StartupError::Filesystem {
            operation: "ensure socket parent directory",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "expected a directory at this path",
            ),
        });
    }
    if metadata.uid() != owner_uid {
        return Err(StartupError::UnsafeOwner {
            path: path.to_path_buf(),
            expected_uid: owner_uid,
            actual_uid: metadata.uid(),
        });
    }
    let mode = metadata.mode() & 0o7777;
    if mode & CUSTOM_SOCKET_PARENT_OTHER_WRITABLE_MASK != 0 {
        return Err(StartupError::UnsafePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

fn custom_parent_needs_private_lock(metadata: &fs::Metadata) -> bool {
    metadata.mode() & CUSTOM_SOCKET_PARENT_GROUP_WRITABLE_MASK != 0
}

fn create_owner_only_directory(path: &Path) -> Result<(), StartupError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(SOCKET_DIRECTORY_MODE);
    builder
        .create(path)
        .map_err(|error| StartupError::Filesystem {
            operation: "create owner-only directory",
            path: path.to_path_buf(),
            source: error,
        })
}

pub(super) fn prepare_socket_path_safe(
    socket_path: &Path,
    owner_uid: u32,
) -> Result<(), StartupError> {
    match fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            let original_identity = FileIdentity::from_metadata(&metadata);
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(StartupError::SymlinkRejected {
                    path: socket_path.to_path_buf(),
                });
            }
            if !file_type.is_socket() {
                return Err(StartupError::Filesystem {
                    operation: "remove non-socket residue",
                    path: socket_path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "endpoint path exists and is not a Unix socket",
                    ),
                });
            }
            if metadata.uid() != owner_uid {
                return Err(StartupError::UnsafeOwner {
                    path: socket_path.to_path_buf(),
                    expected_uid: owner_uid,
                    actual_uid: metadata.uid(),
                });
            }
            if !stale_socket_unanswered(socket_path)? {
                return Err(StartupError::Filesystem {
                    operation: "treat answering socket as stale",
                    path: socket_path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another rmux daemon is already answering this endpoint",
                    ),
                });
            }
            remove_stale_socket_if_still_same(socket_path, owner_uid, original_identity)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StartupError::Filesystem {
            operation: "stat socket path",
            path: socket_path.to_path_buf(),
            source: error,
        }),
    }
}

fn remove_stale_socket_if_still_same(
    socket_path: &Path,
    owner_uid: u32,
    original_identity: FileIdentity,
) -> Result<(), StartupError> {
    let metadata = fs::symlink_metadata(socket_path).map_err(|error| StartupError::Filesystem {
        operation: "re-stat stale socket before removal",
        path: socket_path.to_path_buf(),
        source: error,
    })?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: socket_path.to_path_buf(),
        });
    }
    if !file_type.is_socket() {
        return Err(StartupError::Filesystem {
            operation: "revalidate stale socket before removal",
            path: socket_path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "endpoint path changed and is no longer a Unix socket",
            ),
        });
    }
    if metadata.uid() != owner_uid {
        return Err(StartupError::UnsafeOwner {
            path: socket_path.to_path_buf(),
            expected_uid: owner_uid,
            actual_uid: metadata.uid(),
        });
    }
    if FileIdentity::from_metadata(&metadata) != original_identity {
        return Err(StartupError::Filesystem {
            operation: "revalidate stale socket before removal",
            path: socket_path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::WouldBlock,
                "endpoint path changed while probing stale socket",
            ),
        });
    }

    fs::remove_file(socket_path).map_err(|error| StartupError::Filesystem {
        operation: "remove stale socket",
        path: socket_path.to_path_buf(),
        source: error,
    })
}

fn stale_socket_unanswered(socket_path: &Path) -> Result<bool, StartupError> {
    use std::os::unix::net::UnixStream as StdUnixStream;

    match StdUnixStream::connect(socket_path) {
        Ok(stream) => {
            // Drop the probe stream immediately; we only needed the connect
            // result. The timeout on the closing handshake guards against a
            // peer that accepts but never reads a goodbye frame.
            let _ = stream.set_read_timeout(Some(STALE_PROBE_TIMEOUT));
            drop(stream);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            Ok(true)
        }
        Err(error) => Err(StartupError::Filesystem {
            operation: "probe potentially stale socket",
            path: socket_path.to_path_buf(),
            source: error,
        }),
    }
}
