//! Endpoint selection for callers invoking RMUX as tmux.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use super::{
    claude_swarm_socket_redirect, default_endpoint, endpoint_for_label, endpoint_for_socket_path,
    socket_path_from_rmux_env, socket_path_from_tmux_env, LocalEndpoint, RMUX_ENV, TMUX_ENV,
};
#[cfg(windows)]
use super::{strip_ascii_prefix, PIPE_PREFIX};

/// Resolves the endpoint for tmux-compatible invocation paths.
///
/// Public `rmux` invocations must not consume `$TMUX`; otherwise running RMUX
/// from inside a real tmux client would try to speak the RMUX protocol to a tmux
/// socket. The tmux-compatible path is only for shim/symlink invocations whose
/// caller expects tmux inheritance semantics.
pub fn resolve_tmux_compatible_endpoint(
    socket_name: Option<&OsStr>,
    socket_path: Option<&Path>,
) -> io::Result<LocalEndpoint> {
    if let Some(socket_path) = socket_path {
        return tmux_compatible_endpoint_for_socket_path(socket_path);
    }
    if let Some(socket_name) = socket_name {
        if let Some(endpoint) = claude_swarm_socket_redirect(socket_name) {
            return endpoint;
        }
        return endpoint_for_label(socket_name);
    }
    if let Some(socket_path) = socket_path_from_rmux_env(std::env::var_os(RMUX_ENV).as_deref()) {
        return Ok(LocalEndpoint::from_path(socket_path));
    }
    if let Some(socket_path) = socket_path_from_tmux_env(std::env::var_os(TMUX_ENV).as_deref()) {
        return Ok(LocalEndpoint::from_path(socket_path));
    }
    default_endpoint()
}

/// A tmux caller names a private server by a file path. On Windows that path
/// selects the RMUX-derived pipe of a label made from it, so every spelling of
/// one file (8.3, long, case) names one server and no foreign pipe is reached.
#[cfg(windows)]
fn tmux_compatible_endpoint_for_socket_path(socket_path: &Path) -> io::Result<LocalEndpoint> {
    let value = socket_path.as_os_str().to_string_lossy();
    if value.is_empty() || strip_ascii_prefix(&value, PIPE_PREFIX).is_some() {
        return endpoint_for_socket_path(socket_path);
    }
    let absolute = std::path::absolute(socket_path)?;
    let resolved = match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)?.join(name),
        _ => absolute,
    };
    endpoint_for_label(crate::windows_endpoint_naming::socket_path_label(
        &resolved.to_string_lossy().to_lowercase(),
    ))
}

#[cfg(not(windows))]
fn tmux_compatible_endpoint_for_socket_path(socket_path: &Path) -> io::Result<LocalEndpoint> {
    endpoint_for_socket_path(socket_path)
}
