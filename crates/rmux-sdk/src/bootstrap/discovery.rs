//! SDK runtime endpoint and timeout discovery.
//!
//! This module is intentionally a thin SDK layer over the existing RMUX IPC
//! endpoint defaults. Explicit endpoints stay caller-owned. Only SDK
//! environment endpoint discovery is constrained by the Unix socket allowlist.

use std::env;
use std::io;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::path::PathBuf;

use crate::{Result, RmuxEndpoint, RmuxError};

/// Environment variable for the SDK endpoint override.
pub const SDK_ENDPOINT_ENV: &str = "RMUX_SDK_ENDPOINT";
/// Environment variable for the `rmux` binary used by SDK daemon startup.
pub const SDK_DAEMON_BINARY_ENV: &str = "RMUX_SDK_DAEMON_BINARY";
/// Environment variable for the SDK operation timeout override in milliseconds.
pub const SDK_TIMEOUT_MS_ENV: &str = "RMUX_SDK_TIMEOUT_MS";
/// Default v1 SDK operation timeout.
pub const V1_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolves an SDK endpoint selector for runtime use.
///
/// Explicit [`RmuxEndpoint::UnixSocket`] and [`RmuxEndpoint::WindowsPipe`]
/// values are returned unchanged. [`RmuxEndpoint::Default`] first consults
/// [`SDK_ENDPOINT_ENV`] and then falls through to the platform default from
/// `rmux-ipc`.
pub fn resolve_endpoint(configured: &RmuxEndpoint) -> Result<RmuxEndpoint> {
    match configured {
        RmuxEndpoint::Default => resolve_default_endpoint(),
        endpoint => Ok(endpoint.clone()),
    }
}

/// Resolves the effective timeout for one SDK operation.
///
/// The precedence is per-operation override, builder default, environment
/// override from [`SDK_TIMEOUT_MS_ENV`], then [`V1_DEFAULT_TIMEOUT`].
/// `Duration::MAX` at either explicit layer means no timeout and resolves to
/// `None`.
#[must_use]
pub fn resolve_timeout(
    per_operation_timeout: Option<Duration>,
    builder_default_timeout: Option<Duration>,
) -> Option<Duration> {
    if let Some(timeout) = per_operation_timeout {
        return normalize_timeout(timeout);
    }
    if let Some(timeout) = builder_default_timeout {
        return normalize_timeout(timeout);
    }
    if let Some(timeout) = timeout_from_env() {
        return normalize_timeout(timeout);
    }

    Some(V1_DEFAULT_TIMEOUT)
}

fn resolve_default_endpoint() -> Result<RmuxEndpoint> {
    if let Some(endpoint) = endpoint_from_env() {
        return Ok(endpoint);
    }

    platform_default_endpoint()
}

fn platform_default_endpoint() -> Result<RmuxEndpoint> {
    rmux_ipc::default_endpoint()
        .map(local_endpoint_into_sdk)
        .map_err(endpoint_discovery_error)
}

#[cfg(unix)]
fn local_endpoint_into_sdk(endpoint: rmux_ipc::LocalEndpoint) -> RmuxEndpoint {
    RmuxEndpoint::UnixSocket(endpoint.into_path())
}

#[cfg(windows)]
fn local_endpoint_into_sdk(endpoint: rmux_ipc::LocalEndpoint) -> RmuxEndpoint {
    RmuxEndpoint::WindowsPipe(
        endpoint
            .as_path()
            .as_os_str()
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(unix)]
fn endpoint_from_env() -> Option<RmuxEndpoint> {
    let value = env::var_os(SDK_ENDPOINT_ENV)?;
    let path = unix_path_from_env_value(&value)?;

    unix_auto_socket_path_is_allowed(&path).then_some(RmuxEndpoint::UnixSocket(path))
}

#[cfg(windows)]
fn endpoint_from_env() -> Option<RmuxEndpoint> {
    let value = env::var_os(SDK_ENDPOINT_ENV)?;
    let value = value.to_string_lossy();
    let value = value.trim();

    windows_pipe_is_rmux_owned(value).then(|| RmuxEndpoint::WindowsPipe(value.to_owned()))
}

#[cfg(unix)]
fn unix_path_from_env_value(value: &std::ffi::OsStr) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return None;
    }

    let path = PathBuf::from(value);
    (path.is_absolute()).then_some(path)
}

#[cfg(unix)]
fn unix_auto_socket_path_is_allowed(path: &Path) -> bool {
    socket_parent_is_inside_owned_root(path) && path_has_safe_socket_target(path)
}

#[cfg(unix)]
fn socket_parent_is_inside_owned_root(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(canonical_parent) = std::fs::canonicalize(parent) else {
        return false;
    };
    let Ok(root) = owned_socket_root() else {
        return false;
    };

    canonical_parent.starts_with(root)
}

#[cfg(unix)]
fn owned_socket_root() -> io::Result<PathBuf> {
    let endpoint = rmux_ipc::default_endpoint()?;
    endpoint
        .into_path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "default endpoint has no parent"))
}

#[cfg(unix)]
fn path_has_safe_socket_target(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_socket(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn windows_pipe_is_rmux_owned(value: &str) -> bool {
    const PIPE_PREFIX: &str = r"\\.\pipe\";
    const SOCKET_DIR_PREFIX: &str = "rmux";

    let Some(rest) = strip_ascii_prefix(value, PIPE_PREFIX) else {
        return false;
    };

    rest.starts_with(SOCKET_DIR_PREFIX) && rest[SOCKET_DIR_PREFIX.len()..].starts_with('-')
}

#[cfg(windows)]
fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn timeout_from_env() -> Option<Duration> {
    let value = env::var(SDK_TIMEOUT_MS_ENV).ok()?;
    let millis = value.trim().parse::<u64>().ok()?;

    Some(Duration::from_millis(millis))
}

fn normalize_timeout(timeout: Duration) -> Option<Duration> {
    (timeout != Duration::MAX).then_some(timeout)
}

fn endpoint_discovery_error(error: io::Error) -> RmuxError {
    RmuxError::transport("resolve rmux SDK endpoint", error)
}

#[cfg(test)]
mod tests;
