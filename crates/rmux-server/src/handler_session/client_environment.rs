use std::collections::HashMap;

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
