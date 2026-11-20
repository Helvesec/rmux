use rmux_core::{
    command_parser::{CommandArgument, ParsedCommand},
    command_target_metadata, TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use rmux_proto::{RmuxError, Target};

use super::independence::{
    has_bare_short_option, omitted_source_may_consume_current_target, target_is_independent,
};
use super::{
    normalize_repeated_target_options, preserve_link_window_bare_destination_value,
    queue_target_spec_for_flag, short_option_prefix_len, short_option_takes_next_value,
};
use crate::handler::scripting_support::QueueExecutionContext;
use crate::pane_terminals::HandlerState;

/// A retired event supplies no resolvable current role. Resolve independent
/// roles against an empty context before allowing any ordinary parser to run.
pub(in crate::handler::scripting_support) fn admit_historical_target_roles(
    command: ParsedCommand,
    state: &HandlerState,
    context: &QueueExecutionContext,
) -> Result<(ParsedCommand, TargetFindContext), RmuxError> {
    let marked = state.marked_pane_target();
    let missing = TargetFindContext::new(None)
        .forbid_missing_current_target_fallback()
        .with_mouse_target(context.mouse_target.clone())
        .with_marked_target(marked.clone().map(Target::Pane));
    let missing = crate::handler::with_visible_pane_bases(missing, &state.sessions, &state.options);
    let Some(metadata) = command_target_metadata(command.name()) else {
        return Ok((command, missing));
    };
    // These commands may run work independent of the retired event. Their
    // special binding carries the lease into every child admission instead.
    if matches!(command.name(), "if-shell" | "run-shell" | "source-file") {
        return Ok((command, missing));
    }
    let command = normalize_repeated_target_options(command);
    let arguments = command
        .arguments()
        .iter()
        .map_while(|arg| arg.as_string())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let arguments = &arguments[..short_option_prefix_len(command.name(), &arguments)];
    if role_is_unused(command.name(), arguments) {
        return Ok((command, missing));
    }
    let mut admitted_arguments = command.arguments().to_vec();
    let mut independent_target = None;
    for spec in [metadata.source, metadata.target].into_iter().flatten() {
        let option = format!("-{}", spec.flag);
        let mut value = None;
        let mut value_index = None;
        let mut index = 0;
        while index < arguments.len() {
            if arguments[index] == option {
                value = arguments.get(index + 1).cloned();
                value_index = Some(index + 1);
            }
            index += 1 + usize::from(short_option_takes_next_value(
                command.name(),
                &arguments[index],
            ));
        }
        let is_source = metadata
            .source
            .is_some_and(|source| source.flag == spec.flag);
        if value.is_none()
            && is_source
            && !omitted_source_may_consume_current_target(
                command.name(),
                arguments,
                marked.is_some(),
            )
        {
            continue;
        }
        let Some(value) = value else {
            return Err(retired_role(command.name()));
        };
        let mut spec = queue_target_spec_for_flag(command.name(), spec.flag, &value, arguments)
            .expect("metadata role has a target specification");
        if preserve_link_window_bare_destination_value(command.name(), spec.flag, &value, arguments)
        {
            spec.find_type = TargetFindType::Session;
            spec.flags = TargetFindFlags::NONE;
        }
        // WINDOW_INDEX may choose a default session even when current-target
        // fallback is forbidden. Its relative destinations need the event.
        if spec.flags.contains(TargetFindFlags::WINDOW_INDEX)
            && !target_is_independent(&value, spec.find_type)
            && !value.starts_with(['@', '$', '%'])
        {
            return Err(retired_role(command.name()));
        }
        let target = state
            .sessions
            .resolve_unresolved_target(
                &UnresolvedTarget::new(value),
                spec.find_type,
                spec.flags,
                &missing,
            )
            .map_err(|_| retired_role(command.name()))?;
        if !spec.flags.contains(TargetFindFlags::WINDOW_INDEX) {
            // The next parser resolves visible indices again. Keep a resolved
            // pane absolute so its internal index never becomes a visible one.
            let argument = match &target {
                Target::Pane(pane) => crate::pane_terminal_lookup::pane_id_for_target(
                    &state.sessions,
                    pane.session_name(),
                    pane.window_index(),
                    pane.pane_index(),
                )
                .map_err(|_| retired_role(command.name()))?
                .to_string(),
                _ => target.to_string(),
            };
            admitted_arguments[value_index.expect("explicit role has a value index")] =
                CommandArgument::String(argument);
        }
        independent_target = Some(Target::Session(target.session_name().clone()));
    }
    // Some parsers eagerly compute their unused implicit default even with -t.
    // Seed that default only after every consumed role proved independent.
    let find_context = match independent_target {
        Some(target) => crate::handler::with_visible_pane_bases(
            TargetFindContext::from_target(target)
                .forbid_missing_current_target_fallback()
                .with_mouse_target(context.mouse_target.clone())
                .with_marked_target(marked.map(Target::Pane)),
            &state.sessions,
            &state.options,
        ),
        None => missing,
    };
    Ok((command.with_arguments(admitted_arguments), find_context))
}

fn retired_role(command: &str) -> RmuxError {
    RmuxError::Server(format!(
        "queued {command} lifecycle target retired before parsing"
    ))
}

fn role_is_unused(command: &str, arguments: &[String]) -> bool {
    let flag = |flag| has_bare_short_option(command, arguments, flag);
    match command {
        "set-option" | "show-options" => flag('g') || flag('s'),
        "set-window-option"
        | "show-window-options"
        | "set-environment"
        | "show-environment"
        | "set-hook"
        | "show-hooks" => flag('g'),
        "send-keys" | "display-message" => flag('c') && !flag('t'),
        "list-panes" | "list-windows" => flag('a'),
        _ => false,
    }
}
