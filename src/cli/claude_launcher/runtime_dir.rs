//! Runtime directories and the runner pid file, plus stale Windows temp cleanup.

use std::env;
#[cfg(windows)]
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(any(unix, windows))]
use std::io::Write;
#[cfg(unix)]
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process;
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

use super::ExitFailure;

pub(super) fn write_runner_pid_file(pid_file: &Path) -> Result<(), ExitFailure> {
    #[cfg(unix)]
    {
        write_runner_pid_file_unix(pid_file)
    }

    #[cfg(not(unix))]
    {
        if let Some(parent) = pid_file.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create runner pid directory '{}': {error}",
                        parent.display()
                    ),
                )
            })?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(pid_file)
            .map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create runner pid file '{}': {error}",
                        pid_file.display()
                    ),
                )
            })?;
        file.write_all(format!("{}\n", process::id()).as_bytes())
            .map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to write runner pid file '{}': {error}",
                        pid_file.display()
                    ),
                )
            })
    }
}

#[cfg(unix)]
pub(super) fn create_runtime_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    create_secure_runtime_dir(prefix)
}

#[cfg(windows)]
pub(super) fn create_runtime_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    create_windows_temporary_dir(prefix)
}

#[cfg(unix)]
fn write_runner_pid_file_unix(pid_file: &Path) -> Result<(), ExitFailure> {
    if let Some(parent) = pid_file.parent() {
        validate_secure_owner_directory(parent, "Claude runner pid directory")?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(pid_file)
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to create runner pid file '{}': {error}",
                    pid_file.display()
                ),
            )
        })?;
    file.write_all(format!("{}\n", process::id()).as_bytes())
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to write runner pid file '{}': {error}",
                    pid_file.display()
                ),
            )
        })
}

#[cfg(unix)]
fn create_secure_runtime_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    let temp_root = env::temp_dir();
    for _ in 0..64 {
        let suffix = random_hex_128()?;
        let runtime_dir = temp_root.join(format!("{prefix}-{suffix}"));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&runtime_dir) {
            Ok(()) => {
                validate_secure_owner_directory(&runtime_dir, "Claude runtime directory")?;
                return Ok(runtime_dir);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create runtime directory '{}': {error}",
                        runtime_dir.display()
                    ),
                ));
            }
        }
    }
    Err(ExitFailure::new(
        1,
        "rmux claude: failed to create a unique runtime directory",
    ))
}

#[cfg(unix)]
fn random_hex_128() -> Result<String, ExitFailure> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!("rmux claude: failed to read OS randomness: {error}"),
            )
        })?;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(output)
}

#[cfg(unix)]
pub(super) fn validate_secure_owner_directory(path: &Path, label: &str) -> Result<(), ExitFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to inspect {label} '{}': {error}",
                path.display()
            ),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing symlinked {label} '{}'",
                path.display()
            ),
        ));
    }
    if !metadata.is_dir() {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing non-directory {label} '{}'",
                path.display()
            ),
        ));
    }
    // SAFETY: `geteuid` reads the effective uid of the current process and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing {label} '{}' owned by uid {}",
                path.display(),
                metadata.uid()
            ),
        ));
    }

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 == 0 {
        return Ok(());
    }

    fs::set_permissions(path, fs::Permissions::from_mode(mode & !0o077)).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to tighten permissions on {label} '{}': {error}",
                path.display()
            ),
        )
    })?;
    let tightened = fs::symlink_metadata(path).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to re-inspect {label} '{}': {error}",
                path.display()
            ),
        )
    })?;
    if tightened.mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing group/world-accessible {label} '{}'",
                path.display()
            ),
        ))
    }
}

#[cfg(windows)]
pub(super) fn create_windows_temporary_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    let temp_root = env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    for attempt in 0..64u32 {
        let dir = temp_root.join(format!(
            "{prefix}-{}-{timestamp:x}-{attempt}",
            process::id()
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create temporary directory '{}': {error}",
                        dir.display()
                    ),
                ));
            }
        }
    }
    Err(ExitFailure::new(
        1,
        "rmux claude: failed to create a unique temporary directory",
    ))
}

#[cfg(windows)]
pub(super) fn cleanup_stale_windows_claude_temp_dirs() {
    cleanup_stale_windows_claude_temp_dirs_with_root(&env::temp_dir());
}

#[cfg(windows)]
pub(super) fn cleanup_stale_windows_claude_temp_dirs_with_root(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let Some(pid) = stale_windows_claude_temp_dir_pid(name) else {
            continue;
        };
        if !windows_process_is_running(pid) {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(windows)]
pub(super) fn stale_windows_claude_temp_dir_pid(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix("rmux-claude-shim-")
        .or_else(|| name.strip_prefix("rmux-claude-"))?;
    let pid = rest.split('-').next()?;
    (!pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()
}

#[cfg(windows)]
fn windows_process_is_running(pid: u32) -> bool {
    rmux_os::process::is_live(pid)
}
