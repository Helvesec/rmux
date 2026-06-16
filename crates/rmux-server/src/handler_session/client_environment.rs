use std::collections::HashMap;
use std::ffi::OsString;

use rmux_proto::RmuxError;

use crate::handler::client_environment_snapshot;
use crate::terminal::parse_environment_assignments;

pub(super) fn new_session_client_environment(
    requester_pid: u32,
    request_environment: Option<&[String]>,
) -> Result<Option<HashMap<String, String>>, RmuxError> {
    if let Some(request_environment) = request_environment {
        return parse_environment_assignments(request_environment).map(Some);
    }

    Ok(client_environment_snapshot(requester_pid))
}

#[cfg(any(unix, windows))]
pub(super) fn new_session_raw_client_environment(
    requester_pid: u32,
) -> Option<Vec<(OsString, OsString)>> {
    if requester_pid == std::process::id() {
        return Some(std::env::vars_os().collect());
    }
    rmux_os::process::raw_environment(requester_pid)
}

pub(super) fn raw_environment_from_assignments(
    environment: &HashMap<String, String>,
) -> Vec<(OsString, OsString)> {
    environment
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
}

#[cfg(not(any(unix, windows)))]
pub(super) fn new_session_raw_client_environment(
    _requester_pid: u32,
) -> Option<Vec<(OsString, OsString)>> {
    None
}
