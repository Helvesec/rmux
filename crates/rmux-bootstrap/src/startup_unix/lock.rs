use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::Duration;

use rustix::fs::{flock, FlockOperation};
use tokio::time::sleep;

use super::{StartupError, STARTUP_LOCK_MODE, UNSAFE_PERMISSION_MASK};
use crate::deadline::StartupDeadline;

#[derive(Debug)]
pub(super) struct StartupLock {
    file: fs::File,
}

impl StartupLock {
    pub(super) async fn acquire(
        path: &Path,
        owner_uid: u32,
        deadline: StartupDeadline,
        poll_interval: Duration,
    ) -> Result<Self, StartupError> {
        loop {
            let file = open_validated_lock_file(path, owner_uid)?;
            acquire_lock_with_deadline(path, &file, deadline, poll_interval).await?;

            let metadata = file.metadata().map_err(|error| StartupError::Lock {
                path: path.to_path_buf(),
                source: error,
            })?;
            validate_lock_metadata(path, &metadata, owner_uid)?;
            // A releasing holder may unlink the lock file before it unlocks, as
            // an exiting daemon does. The lock then held belongs to a file that
            // no longer serializes anything: lock the path's current file.
            if locked_file_is_still_named(path, &metadata)? {
                return Ok(Self { file });
            }
            if deadline.is_elapsed() {
                return Err(StartupError::Lock {
                    path: path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "startup lock file changed while acquiring lock",
                    ),
                });
            }
        }
    }
}

fn open_validated_lock_file(path: &Path, owner_uid: u32) -> Result<fs::File, StartupError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        validate_lock_metadata(path, &metadata, owner_uid)?;
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .mode(STARTUP_LOCK_MODE)
        .open(path)
        .map_err(|error| StartupError::Lock {
            path: path.to_path_buf(),
            source: error,
        })?;

    let metadata = file.metadata().map_err(|error| StartupError::Lock {
        path: path.to_path_buf(),
        source: error,
    })?;
    validate_lock_metadata(path, &metadata, owner_uid)?;
    Ok(file)
}

fn validate_lock_metadata(
    path: &Path,
    metadata: &fs::Metadata,
    owner_uid: u32,
) -> Result<(), StartupError> {
    if metadata.file_type().is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: path.to_path_buf(),
        });
    }
    if !metadata.file_type().is_file() {
        return Err(StartupError::Filesystem {
            operation: "validate lock file is a regular file",
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "startup lock path is not a regular file",
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
    if mode & UNSAFE_PERMISSION_MASK != 0 {
        return Err(StartupError::UnsafePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

async fn acquire_lock_with_deadline(
    path: &Path,
    file: &fs::File,
    deadline: StartupDeadline,
    poll_interval: Duration,
) -> Result<(), StartupError> {
    const MIN_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(1);

    let effective_poll = poll_interval.max(MIN_LOCK_POLL_INTERVAL);

    loop {
        match flock(file.as_fd(), FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let source = io::Error::from(error);
                if source.kind() != io::ErrorKind::WouldBlock {
                    return Err(StartupError::Lock {
                        path: path.to_path_buf(),
                        source,
                    });
                }

                if deadline.is_elapsed() {
                    return Err(StartupError::Lock {
                        path: path.to_path_buf(),
                        source: io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "timed out after {}ms waiting for startup lock",
                                deadline.elapsed().as_millis()
                            ),
                        ),
                    });
                }

                sleep(deadline.sleep_for(effective_poll)).await;
            }
        }
    }
}

fn locked_file_is_still_named(
    path: &Path,
    locked_metadata: &fs::Metadata,
) -> Result<bool, StartupError> {
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(StartupError::Lock {
                path: path.to_path_buf(),
                source: error,
            })
        }
    };
    if path_metadata.file_type().is_symlink() {
        return Err(StartupError::SymlinkRejected {
            path: path.to_path_buf(),
        });
    }
    Ok(
        path_metadata.dev() == locked_metadata.dev()
            && path_metadata.ino() == locked_metadata.ino(),
    )
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the flock; the explicit unlock
        // makes the release point obvious to anyone tracing the lock.
        let _ = flock(self.file.as_fd(), FlockOperation::Unlock);
    }
}
