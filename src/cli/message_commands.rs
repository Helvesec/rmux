use std::path::Path;

use rmux_client::connect;
use rmux_proto::{ResolveTargetType, Response, Target};
use serde_json::json;

use super::command_runner::{
    finish_command_success, inherited_pane_target, run_queued_server_command, write_command_output,
};
use super::json_output::{stdout_string, write_json_object};
use super::{expect_command_output, resolve_target_spec, unexpected_response, ExitFailure};
use crate::cli_args::{parse_target_spec, DisplayMessageArgs};

pub(super) fn run_display_message(
    args: DisplayMessageArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if args.json {
        return run_display_message_json(args, socket_path);
    }
    if !display_message_can_use_direct_request(&args) {
        return run_queued_server_command(socket_path, "display-message", args.queue_command);
    }

    run_display_message_direct(args, socket_path)
}

pub(super) fn run_display_message_json(
    args: DisplayMessageArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_display_message_json_inner(args, socket_path)
}

fn run_display_message_json_inner(
    args: DisplayMessageArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target =
        resolve_display_message_target(&mut connection, socket_path, args.target.as_deref())?;
    let message = display_message_template(&args)?;
    let response = if args.target_client.is_some() {
        connection.display_message_ext(target, true, message, args.target_client)
    } else {
        connection.display_message(target, true, message)
    }
    .map_err(ExitFailure::from_client)?;

    match response {
        Response::DisplayMessage(response) => {
            let message = response
                .command_output()
                .map(|output| stdout_string(output.stdout(), "display-message"))
                .transpose()?
                .unwrap_or_default();
            write_json_object(
                &json!({ "message": trim_one_trailing_newline(&message) }),
                "display-message",
            )
        }
        Response::Error(error) => Err(ExitFailure::new(1, error.error.to_string())),
        other => Err(unexpected_response("display-message", &other)),
    }
}

fn run_display_message_direct(
    args: DisplayMessageArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target =
        resolve_display_message_target(&mut connection, socket_path, args.target.as_deref())?;
    let message = display_message_template(&args)?;
    let response = if args.target_client.is_some() {
        connection.display_message_ext(target, args.print, message, args.target_client)
    } else {
        connection.display_message(target, args.print, message)
    }
    .map_err(ExitFailure::from_client)?;

    if args.print {
        let output = expect_command_output(&response, "display-message")?;
        write_command_output(output)?;
        Ok(0)
    } else {
        finish_command_success(response, "display-message")
    }
}

fn display_message_can_use_direct_request(args: &DisplayMessageArgs) -> bool {
    !args.all_formats && !args.stdin && !args.literal && !args.verbose && args.target.is_none()
}

fn resolve_display_message_target(
    connection: &mut rmux_client::Connection,
    socket_path: &Path,
    target: Option<&str>,
) -> Result<Option<rmux_proto::Target>, ExitFailure> {
    match target {
        Some(target) => {
            let target = parse_target_spec(target).map_err(|error| ExitFailure::new(1, error))?;
            resolve_target_spec(connection, &target, ResolveTargetType::Pane, false, false)
                .map(Some)
        }
        None => {
            inherited_pane_target(connection, socket_path).map(|target| target.map(Target::Pane))
        }
    }
}

fn display_message_template(args: &DisplayMessageArgs) -> Result<Option<String>, ExitFailure> {
    if args.format.is_some() && !args.message.is_empty() {
        return Err(ExitFailure::new(
            1,
            "only one of -F or argument must be given",
        ));
    }
    Ok(args
        .format
        .clone()
        .or_else(|| (!args.message.is_empty()).then(|| args.message.join(" "))))
}

fn trim_one_trailing_newline(value: &str) -> &str {
    value.strip_suffix('\n').unwrap_or(value)
}
