use std::path::Path;

use rmux_client::Connection;
use rmux_proto::{ListClientsRequest, SessionName};

use super::command_runner::run_command_resolved;
use super::target_resolution::resolve_current_session_target;
use super::{expect_command_output, ExitFailure};
use crate::cli_args::DisplayPanesArgs;

pub(super) fn run_display_panes(
    args: DisplayPanesArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let template = args.template_command();
    run_command_resolved(socket_path, "display-panes", move |connection| {
        let target = client_session(connection, args.target_client.as_deref())?;
        connection
            .display_panes_target_client(
                target,
                args.duration_ms,
                args.non_blocking,
                args.no_command,
                template,
                args.target_client,
            )
            .map_err(ExitFailure::from_client)
    })
}

fn client_session(
    connection: &mut Connection,
    client: Option<&str>,
) -> Result<SessionName, ExitFailure> {
    let Some(client) = client.map(|client| client.strip_suffix(':').unwrap_or(client)) else {
        return resolve_current_session_target(connection);
    };
    if client == "=" {
        return resolve_current_session_target(connection);
    }
    let response = connection.list_clients(ListClientsRequest {
        format: Some("#{client_pid}\u{1f}#{client_name}\u{1f}#{client_session}\u{1f}#{client_control_mode}\u{1e}".to_owned()),
        filter: None, sort_order: None, reversed: false, target_session: None,
    }).map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-clients")?;
    let rendered = String::from_utf8_lossy(output.stdout());
    for row in rendered.split('\u{1e}') {
        let row = row.strip_prefix('\n').unwrap_or(row);
        let fields: Vec<_> = row.split('\u{1f}').collect();
        let [pid, name, session, control] = fields.as_slice() else {
            continue;
        };
        if *control == "1" {
            continue;
        }
        let matches = match client.parse::<u32>() {
            Ok(client_pid) => pid.parse::<u32>() == Ok(client_pid),
            Err(_) => {
                let path = Path::new(name);
                path == Path::new(client)
                    || path
                        .strip_prefix("/dev")
                        .is_ok_and(|path| path == Path::new(client))
            }
        };
        if matches {
            return serde_json::from_value(serde_json::Value::String((*session).to_owned()))
                .map_err(|error| ExitFailure::new(1, error.to_string()));
        }
    }
    // Preserve the handler's established missing/control-client diagnostic.
    resolve_current_session_target(connection)
}
