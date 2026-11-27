use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::StartupError;

#[derive(Debug)]
pub(super) struct ParentAnchor {
    path: PathBuf,
    file: fs::File,
    identity: FileIdentity,
}

impl ParentAnchor {
    pub(super) fn open(path: &Path) -> Result<Self, StartupError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|error| StartupError::Filesystem {
                operation: "open socket parent directory anchor",
                path: path.to_path_buf(),
                source: error,
            })?;
        let metadata = file.metadata().map_err(|error| StartupError::Filesystem {
            operation: "stat socket parent directory anchor",
            path: path.to_path_buf(),
            source: error,
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity: FileIdentity::from_metadata(&metadata),
        })
    }

    pub(super) fn validate(&self, operation: &'static str) -> Result<(), StartupError> {
        let fd_metadata = self
            .file
            .metadata()
            .map_err(|error| StartupError::Filesystem {
                operation,
                path: self.path.clone(),
                source: error,
            })?;
        if FileIdentity::from_metadata(&fd_metadata) != self.identity {
            return Err(parent_changed_error(operation, &self.path));
        }

        let path_metadata =
            fs::symlink_metadata(&self.path).map_err(|error| StartupError::Filesystem {
                operation,
                path: self.path.clone(),
                source: error,
            })?;
        if path_metadata.file_type().is_symlink() {
            return Err(StartupError::SymlinkRejected {
                path: self.path.clone(),
            });
        }
        if !path_metadata.file_type().is_dir()
            || FileIdentity::from_metadata(&path_metadata) != self.identity
        {
            return Err(parent_changed_error(operation, &self.path));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    pub(super) fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn parent_changed_error(operation: &'static str, path: &Path) -> StartupError {
    StartupError::Filesystem {
        operation,
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::WouldBlock,
            "socket parent directory changed while starting daemon",
        ),
    }
}
