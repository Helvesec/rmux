use rmux_core::{command_parser::ParsedCommands, TargetFindType};
use rmux_proto::{request::SwitchClientExt3Request, Request};

use super::super::switch_client_target_find_type;
use super::client_parse::{parse_detach_client, parse_switch_client};
use super::tokens::CommandTokens;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::handler) enum ReadOnlyClientAction {
    DetachSelf,
    SwitchSelf(SwitchClientExt3Request),
}

pub(in crate::handler) fn read_only_client_action(
    commands: &ParsedCommands,
) -> Option<ReadOnlyClientAction> {
    if !commands.assignments().is_empty() {
        return None;
    }
    let [command] = commands.commands() else {
        return None;
    };
    let arguments = command
        .arguments()
        .iter()
        .map(|argument| argument.as_string().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;

    match command.name() {
        "detach-client" => {
            classify_detach(parse_detach_client(CommandTokens::new(arguments)).ok()?)
        }
        "switch-client" => {
            classify_switch(parse_switch_client(CommandTokens::new(arguments)).ok()?)
        }
        _ => None,
    }
}

fn classify_detach(request: Request) -> Option<ReadOnlyClientAction> {
    let Request::DetachClientExt(request) = request else {
        return None;
    };
    (request.target_client.is_none()
        && !request.all_other_clients
        && request.target_session.is_none()
        && !request.kill_on_detach
        && request.exec_command.is_none())
    .then_some(ReadOnlyClientAction::DetachSelf)
}

fn classify_switch(request: Request) -> Option<ReadOnlyClientAction> {
    let Request::SwitchClientExt3(request) = request else {
        return None;
    };
    let mut request = *request;
    if request.target_client.is_some() || request.toggle_read_only || request.zoom {
        return None;
    }
    if request
        .target
        .as_deref()
        .is_some_and(|target| switch_client_target_find_type(target) != TargetFindType::Session)
    {
        return None;
    }
    if request.sort_order.is_some() && !(request.next_session || request.previous_session) {
        return None;
    }
    if request.target.is_none()
        && !request.last_session
        && !request.next_session
        && !request.previous_session
        && request.key_table.is_none()
    {
        return None;
    }

    // Updating a target session's environment would cross the client-local
    // authorization boundary even though tmux does so without an explicit -E.
    request.skip_environment_update = true;
    Some(ReadOnlyClientAction::SwitchSelf(request))
}
