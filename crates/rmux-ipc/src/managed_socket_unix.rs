use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::LocalEndpoint;

const SOCKET_DIRECTORY_PREFIX: &str = "rmux";
const OWNER_ONLY_DIRECTORY_MODE: u32 = 0o700;
const ALLOWLISTED_DIRECTORY_MODE: u32 = 0o711;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
pub(crate) struct ManagedSocketDirectoryGuard {
    path: PathBuf,
    identity: Option<DirectoryIdentity>,
}

impl ManagedSocketDirectoryGuard {
    pub(crate) fn before_connect(endpoint: &LocalEndpoint) -> io::Result<Option<Self>> {
        if !endpoint.is_filesystem_path() {
            return Ok(None);
        }
        let Some(path) = managed_socket_directory(endpoint.as_path()) else {
            return Ok(None);
        };
        let identity = match validate_managed_socket_directory(&path) {
            Ok(identity) => Some(identity),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Some(Self { path, identity }))
    }

    pub(crate) fn revalidate(&self) -> io::Result<()> {
        let identity = validate_managed_socket_directory(&self.path)?;
        if self
            .identity
            .is_some_and(|expected_identity| identity != expected_identity)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "managed socket directory '{}' changed while connecting",
                    self.path.display()
                ),
            ));
        }
        Ok(())
    }
}

fn managed_socket_directory(socket_path: &Path) -> Option<PathBuf> {
    let expected = format!(
        "{SOCKET_DIRECTORY_PREFIX}-{}",
        rmux_os::identity::real_user_id()
    );
    socket_path
        .parent()?
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(OsStr::new(&expected)))
        .map(Path::to_path_buf)
}

fn validate_managed_socket_directory(path: &Path) -> io::Result<DirectoryIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "managed socket directory '{}' is not a plain directory",
                path.display()
            ),
        ));
    }

    let expected_uid = rmux_os::identity::real_user_id();
    if metadata.uid() != expected_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "managed socket directory '{}' has unsafe ownership",
                path.display()
            ),
        ));
    }

    let mode = metadata.permissions().mode() & 0o777;
    if mode != OWNER_ONLY_DIRECTORY_MODE && mode != ALLOWLISTED_DIRECTORY_MODE {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "managed socket directory '{}' has unsafe permissions",
                path.display()
            ),
        ));
    }

    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}
