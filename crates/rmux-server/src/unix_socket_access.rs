use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode};

use crate::unix_socket::{
    real_user_id, SocketFileIdentity, UnixTransportAccess, OWNER_ONLY_SOCKET_MODE,
    SHARED_SOCKET_MODE,
};

const GROUP_AND_OTHER_TRAVERSE_MASK: u32 = 0o011;
const GROUP_AND_OTHER_WRITE_MASK: u32 = 0o022;
const STICKY_MODE: u32 = 0o1000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct TransportModes {
    directories: Vec<u32>,
    socket: u32,
}

#[derive(Debug)]
struct DirectoryEndpoint {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    owner: u32,
    private_mode: u32,
    managed_by_rmux: bool,
}

#[derive(Debug)]
struct FilesystemEndpoint {
    socket_name: OsString,
    socket_identity: SocketFileIdentity,
    directories: Vec<DirectoryEndpoint>,
    owner_uid: u32,
}

#[derive(Debug)]
pub(crate) struct UnixSocketAccessController {
    endpoint: Option<FilesystemEndpoint>,
    access: UnixTransportAccess,
    #[cfg(test)]
    fail_after_first_mode_change: bool,
}

impl UnixSocketAccessController {
    pub(crate) fn new(
        socket_path: &Path,
        socket_identity: Option<SocketFileIdentity>,
    ) -> io::Result<Self> {
        if socket_path.as_os_str().is_empty() {
            return Ok(Self {
                endpoint: None,
                access: UnixTransportAccess::OwnerOnly,
                #[cfg(test)]
                fail_after_first_mode_change: false,
            });
        }

        let socket_identity = socket_identity.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "filesystem Unix socket is missing its bound identity",
            )
        })?;
        let parent = rmux_os::path::parent_or_current(socket_path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("socket path '{}' has no parent", socket_path.display()),
            )
        })?;
        let directory_path = fs::canonicalize(parent)?;
        let managed_directory = crate::unix_socket::managed_rmux_socket_directory(&directory_path)?;
        let directories = directory_path
            .ancestors()
            .map(|path| {
                let managed_by_rmux = managed_directory
                    .as_ref()
                    .is_some_and(|managed| path.starts_with(managed));
                DirectoryEndpoint::open(path, managed_by_rmux)
            })
            .collect::<io::Result<Vec<_>>>()?;
        let socket_name = socket_path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("socket path '{}' has no file name", socket_path.display()),
            )
        })?;
        let endpoint = FilesystemEndpoint {
            socket_name: socket_name.to_os_string(),
            socket_identity,
            directories,
            owner_uid: real_user_id()?,
        };
        endpoint.validate_modes(&endpoint.expected_modes(UnixTransportAccess::OwnerOnly))?;

        Ok(Self {
            endpoint: Some(endpoint),
            access: UnixTransportAccess::OwnerOnly,
            #[cfg(test)]
            fail_after_first_mode_change: false,
        })
    }

    #[must_use]
    pub(crate) const fn access(&self) -> UnixTransportAccess {
        self.access
    }

    pub(crate) fn validate_rebind_source(&self) -> io::Result<()> {
        let Some(endpoint) = &self.endpoint else {
            return Ok(());
        };
        let expected = endpoint.expected_modes(self.access);
        endpoint.validate_directories(&expected)?;
        match endpoint.validate_socket(&expected) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn transition(&mut self, target: UnixTransportAccess) -> io::Result<()> {
        if self.endpoint.is_none() {
            self.access = target;
            return Ok(());
        }
        let fail_after_first_change = self.take_test_failure();
        let endpoint = self
            .endpoint
            .as_ref()
            .expect("filesystem endpoint checked above");
        let previous = endpoint.expected_modes(self.access);
        endpoint.validate_modes(&previous)?;
        if self.access == target {
            return Ok(());
        }
        endpoint.validate_transition(target)?;
        let desired = endpoint.expected_modes(target);
        if let Err(error) = endpoint.write_modes(&desired, target, fail_after_first_change) {
            return Err(endpoint.rollback_error(&previous, self.access, error));
        }
        if let Err(error) = endpoint.validate_modes(&desired) {
            return Err(endpoint.rollback_error(&previous, self.access, error));
        }
        self.access = target;
        Ok(())
    }

    pub(crate) fn adopt_rebound_socket(
        &mut self,
        socket_identity: Option<SocketFileIdentity>,
    ) -> io::Result<()> {
        let Some(endpoint) = self.endpoint.as_mut() else {
            if socket_identity.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "abstract Unix endpoint unexpectedly rebound to a filesystem socket",
                ));
            }
            return Ok(());
        };
        let socket_identity = socket_identity.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "rebound filesystem socket is missing its identity",
            )
        })?;
        let previous_identity = endpoint.socket_identity;
        endpoint.socket_identity = socket_identity;
        if let Err(error) = endpoint.validate_modes(&endpoint.expected_modes(self.access)) {
            endpoint.socket_identity = previous_identity;
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_transition_after_first_mode_change(&mut self) {
        self.fail_after_first_mode_change = true;
    }

    fn take_test_failure(&mut self) -> bool {
        #[cfg(test)]
        {
            std::mem::take(&mut self.fail_after_first_mode_change)
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

impl FilesystemEndpoint {
    fn expected_modes(&self, access: UnixTransportAccess) -> TransportModes {
        let directories = self
            .directories
            .iter()
            .map(|directory| match access {
                UnixTransportAccess::OwnerOnly => directory.private_mode,
                UnixTransportAccess::AllowListed if directory.managed_by_rmux => {
                    directory.private_mode | GROUP_AND_OTHER_TRAVERSE_MASK
                }
                UnixTransportAccess::AllowListed => directory.private_mode,
            })
            .collect();
        let socket = match access {
            UnixTransportAccess::OwnerOnly => OWNER_ONLY_SOCKET_MODE,
            UnixTransportAccess::AllowListed => SHARED_SOCKET_MODE,
        };
        TransportModes {
            directories,
            socket,
        }
    }

    fn validate_modes(&self, expected: &TransportModes) -> io::Result<()> {
        self.validate_directories(expected)?;
        self.validate_socket(expected)
    }

    fn validate_directories(&self, expected: &TransportModes) -> io::Result<()> {
        if expected.directories.len() != self.directories.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unix transport directory policy has the wrong length",
            ));
        }
        for (directory, expected_mode) in self.directories.iter().zip(&expected.directories) {
            directory.validate(*expected_mode)?;
        }
        Ok(())
    }

    fn validate_socket(&self, expected: &TransportModes) -> io::Result<()> {
        let socket = self.socket_stat()?;
        if FileType::from_raw_mode(socket.st_mode) != FileType::Socket
            || socket.st_uid != self.owner_uid
            || portable_stat_device(&socket) != self.socket_identity.device
            || socket.st_ino != self.socket_identity.inode
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Unix socket changed type, owner, or identity",
            ));
        }
        if portable_stat_mode(&socket) & 0o777 != expected.socket {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Unix socket has unexpected permissions",
            ));
        }
        Ok(())
    }

    fn validate_transition(&self, target: UnixTransportAccess) -> io::Result<()> {
        if target != UnixTransportAccess::AllowListed {
            return Ok(());
        }
        for directory in &self.directories {
            if directory.private_mode & GROUP_AND_OTHER_WRITE_MASK != 0
                && directory.private_mode & STICKY_MODE == 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "socket path ancestor '{}' is writable by group or others",
                        directory.path.display()
                    ),
                ));
            }
            let needs_traverse = directory.private_mode & GROUP_AND_OTHER_TRAVERSE_MASK
                != GROUP_AND_OTHER_TRAVERSE_MASK;
            if needs_traverse && (!directory.managed_by_rmux || directory.owner != self.owner_uid) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "socket path ancestor '{}' cannot be opened for allowlisted users",
                        directory.path.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn write_modes(
        &self,
        desired: &TransportModes,
        target: UnixTransportAccess,
        fail_after_first_change: bool,
    ) -> io::Result<()> {
        let current = TransportModes {
            directories: self
                .directories
                .iter()
                .map(DirectoryEndpoint::mode)
                .collect::<io::Result<Vec<_>>>()?,
            socket: portable_stat_mode(&self.socket_stat()?) & 0o777,
        };
        let mut changed = false;
        match target {
            UnixTransportAccess::AllowListed => {
                changed |= self.set_socket_mode_if_needed(current.socket, desired.socket)?;
                Self::inject_failure(fail_after_first_change, changed)?;
                for ((directory, current_mode), target_mode) in self
                    .directories
                    .iter()
                    .zip(current.directories)
                    .zip(&desired.directories)
                {
                    changed |= directory.set_mode_if_needed(current_mode, *target_mode)?;
                }
            }
            UnixTransportAccess::OwnerOnly => {
                for ((directory, current_mode), target_mode) in self
                    .directories
                    .iter()
                    .zip(current.directories)
                    .zip(&desired.directories)
                {
                    changed |= directory.set_mode_if_needed(current_mode, *target_mode)?;
                    Self::inject_failure(fail_after_first_change, changed)?;
                }
                changed |= self.set_socket_mode_if_needed(current.socket, desired.socket)?;
            }
        }
        let _ = changed;
        Ok(())
    }

    fn rollback_error(
        &self,
        previous: &TransportModes,
        previous_access: UnixTransportAccess,
        original: io::Error,
    ) -> io::Error {
        match self
            .write_modes(previous, previous_access, false)
            .and_then(|()| self.validate_modes(previous))
        {
            Ok(()) => original,
            Err(rollback) => io::Error::new(
                original.kind(),
                format!("{original}; permission rollback also failed: {rollback}"),
            ),
        }
    }

    fn set_socket_mode_if_needed(&self, current: u32, target: u32) -> io::Result<bool> {
        if current == target {
            return Ok(false);
        }
        rustix::fs::chmodat(
            &self.directories[0].file,
            &self.socket_name,
            Mode::from_raw_mode(target as _),
            AtFlags::empty(),
        )
        .map_err(errno_to_io)?;
        Ok(true)
    }

    fn socket_stat(&self) -> io::Result<rustix::fs::Stat> {
        rustix::fs::statat(
            &self.directories[0].file,
            &self.socket_name,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(errno_to_io)
    }

    fn inject_failure(enabled: bool, changed: bool) -> io::Result<()> {
        if enabled && changed {
            return Err(io::Error::other(
                "injected Unix transport permission failure",
            ));
        }
        Ok(())
    }
}

fn portable_stat_device(stat: &rustix::fs::Stat) -> u64 {
    // libc's dev_t width differs across supported Unix targets. The cast is
    // redundant on Linux but required on macOS and some BSDs.
    #[allow(clippy::unnecessary_cast)]
    {
        stat.st_dev as u64
    }
}

fn portable_stat_mode(stat: &rustix::fs::Stat) -> u32 {
    // libc's mode_t width differs across supported Unix targets. Keep the
    // transport policy represented as u32 without target-specific branches.
    #[allow(clippy::unnecessary_cast)]
    {
        stat.st_mode as u32
    }
}

impl DirectoryEndpoint {
    fn open(path: &Path, managed_by_rmux: bool) -> io::Result<Self> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "socket path ancestor '{}' is not a directory",
                    path.display()
                ),
            ));
        }
        let private_mode = metadata.permissions().mode() & 0o7777;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            private_mode,
            managed_by_rmux,
        })
    }

    fn mode(&self) -> io::Result<u32> {
        Ok(self.file.metadata()?.permissions().mode() & 0o7777)
    }

    fn validate(&self, expected_mode: u32) -> io::Result<()> {
        let metadata = self.file.metadata()?;
        if !metadata.is_dir()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.uid() != self.owner
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "socket directory '{}' changed identity",
                    self.path.display()
                ),
            ));
        }
        if metadata.permissions().mode() & 0o7777 != expected_mode {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "socket directory '{}' has unexpected permissions",
                    self.path.display()
                ),
            ));
        }
        Ok(())
    }

    fn set_mode_if_needed(&self, current: u32, target: u32) -> io::Result<bool> {
        if current == target {
            return Ok(false);
        }
        self.file
            .set_permissions(fs::Permissions::from_mode(target))?;
        Ok(true)
    }
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}
