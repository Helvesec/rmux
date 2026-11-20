use super::{wait_target, ExitFailure, StableWaitTarget};
use crate::cli_args::SendKeysArgs;
use crate::cli_response::tmux_cli_error_message;
use rmux_client::Connection;
use rmux_proto::{
    ErrorResponse, ListClientsRequest, PaneTarget, ResolveTargetType, Response,
    SendKeysExt2Request, SendKeysExtRequest, Target,
};

const CLIENT_FIELD_SEPARATOR: char = '\u{1f}';
const CLIENT_ROW_SEPARATOR: char = '\u{1e}';
const CLIENT_TARGET_FORMAT: &str =
    "#{client_name}\u{1f}#{client_tty}\u{1f}#{client_pid}\u{1f}#{client_session}\u{1f}#{client_control_mode}\u{1e}";

pub(super) struct SendKeysTargetPlan {
    pub(super) send_target: Option<PaneTarget>,
    pub(super) wait_target: Option<StableWaitTarget>,
}

pub(super) fn send_keys_target_plan(
    connection: &mut Connection,
    args: &SendKeysArgs,
) -> Result<SendKeysTargetPlan, ExitFailure> {
    let send_target = if args.target.is_some() {
        Some(super::super::common::resolve_pane_slot(
            connection,
            args.target.as_ref(),
            "send-keys",
        )?)
    } else {
        None
    };
    let wait_target = if let Some(target) = send_target.as_ref() {
        Some(wait_target::for_slot(connection, target, "send-keys")?)
    } else if let Some(target_client) = args.client_target.as_deref() {
        attached_target_client_pane_ref(connection, target_client)?
    } else {
        let current = super::super::common::resolve_pane_slot(connection, None, "send-keys")?;
        Some(wait_target::for_slot(connection, &current, "send-keys")?)
    };

    Ok(SendKeysTargetPlan {
        send_target,
        wait_target,
    })
}

fn attached_target_client_pane_ref(
    connection: &mut Connection,
    target_client: &str,
) -> Result<Option<StableWaitTarget>, ExitFailure> {
    let Some(session_name) = attached_target_client_session(connection, target_client)? else {
        return Ok(None);
    };
    let response = connection
        .resolve_target(Some(session_name), ResolveTargetType::Pane, false, false)
        .map_err(ExitFailure::from_client)?;
    let target = match response {
        Response::ResolveTarget(response) => match response.target {
            Target::Pane(target) => target,
            other => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "resolve-target produced {} where a pane target was required",
                        target_kind_name(&other)
                    ),
                ));
            }
        },
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(
                1,
                tmux_cli_error_message("send-keys", &error),
            ));
        }
        other => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "protocol error: unexpected '{}' response for send-keys",
                    other.command_name()
                ),
            ));
        }
    };

    wait_target::for_slot(connection, &target, "send-keys").map(Some)
}

pub(super) fn unresolved_wait_target_error(args: &SendKeysArgs) -> ExitFailure {
    if let Some(target_client) = args.client_target.as_deref() {
        return ExitFailure::new(
            1,
            format!("send-keys --wait cannot observe a pane for target client {target_client:?}"),
        );
    }
    ExitFailure::new(1, "send-keys --wait cannot resolve a pane to observe")
}

fn attached_target_client_session(
    connection: &mut Connection,
    target_client: &str,
) -> Result<Option<String>, ExitFailure> {
    let response = connection
        .list_clients(ListClientsRequest {
            format: Some(CLIENT_TARGET_FORMAT.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_session: None,
        })
        .map_err(ExitFailure::from_client)?;
    let output = match response {
        Response::ListClients(response) => response.output,
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(
                1,
                tmux_cli_error_message("send-keys", &error),
            ));
        }
        other => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "protocol error: unexpected '{}' response for send-keys",
                    other.command_name()
                ),
            ));
        }
    };
    let target_client = normalize_target_client(target_client);
    if target_client == "=" {
        return Ok(None);
    }
    let rendered = String::from_utf8_lossy(output.stdout());
    for row in rendered.split(CLIENT_ROW_SEPARATOR) {
        let Some(client) = ParsedClientRow::parse(row) else {
            continue;
        };
        if client.control || !client.matches(target_client) {
            continue;
        }
        if client.session.is_empty() {
            return Ok(None);
        }
        return Ok(Some(client.session.to_owned()));
    }
    Ok(None)
}

struct ParsedClientRow<'a> {
    name: &'a str,
    tty: &'a str,
    pid: &'a str,
    session: &'a str,
    control: bool,
}

impl<'a> ParsedClientRow<'a> {
    fn parse(row: &'a str) -> Option<Self> {
        if row.is_empty() {
            return None;
        }
        let mut fields = row.split(CLIENT_FIELD_SEPARATOR);
        let name = fields.next()?;
        let tty = fields.next()?;
        let pid = fields.next()?;
        let session = fields.next()?;
        let control = fields.next()? == "1";
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            name,
            tty,
            pid,
            session,
            control,
        })
    }

    fn matches(&self, target_client: &str) -> bool {
        self.pid == target_client
            || client_path_matches(self.name, target_client)
            || client_path_matches(self.tty, target_client)
    }
}

fn client_path_matches(path: &str, target_client: &str) -> bool {
    path == target_client
        || path
            .strip_prefix("/dev/")
            .is_some_and(|stripped| stripped == target_client)
}

fn normalize_target_client(target_client: &str) -> &str {
    target_client.strip_suffix(':').unwrap_or(target_client)
}

fn target_kind_name(target: &Target) -> &'static str {
    match target {
        Target::Session(_) => "session",
        Target::Window(_) => "window",
        Target::Pane(_) => "pane",
    }
}

pub(super) fn send_keys_through_command_path(
    connection: &mut Connection,
    args: SendKeysArgs,
    target: Option<PaneTarget>,
) -> Result<Response, ExitFailure> {
    if let Some(target_client) = args.client_target {
        return connection
            .send_keys_extended_target_client(SendKeysExt2Request {
                target,
                keys: args.keys,
                expand_formats: args.expand_formats,
                hex: args.hex,
                literal: args.literal,
                dispatch_key_table: args.key_table,
                copy_mode_command: args.copy_mode,
                forward_mouse_event: args.mouse,
                reset_terminal: args.reset_terminal,
                repeat_count: args.repeat_count,
                target_client: Some(target_client),
            })
            .map_err(ExitFailure::from_client);
    }

    connection
        .send_keys_extended(SendKeysExtRequest {
            target,
            keys: args.keys,
            expand_formats: args.expand_formats,
            hex: args.hex,
            literal: args.literal,
            dispatch_key_table: args.key_table,
            copy_mode_command: args.copy_mode,
            forward_mouse_event: args.mouse,
            reset_terminal: args.reset_terminal,
            repeat_count: args.repeat_count,
        })
        .map_err(ExitFailure::from_client)
}
