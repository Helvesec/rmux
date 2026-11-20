//! Whether a command's targets are independent of the inherited current target.

use rmux_core::{command_parser::ParsedCommand, command_target_metadata, TargetFindType};
use rmux_proto::PaneTarget;

use super::{
    normalize_repeated_target_options, short_flag_argument_parts, short_option_prefix_len,
    short_option_takes_next_value,
};

pub(in crate::handler::scripting_support) fn command_has_independent_target(
    command: &ParsedCommand,
    marked_target: Option<&PaneTarget>,
) -> bool {
    // A present mark must be a PaneId-validated target resolved while the
    // HandlerState used for parsing is locked. This keeps DEFAULT_MARKED
    // source selection and inherited-current admission in one snapshot.
    let Some(metadata) = command_target_metadata(command.name()) else {
        return false;
    };
    let Some(target_spec) = metadata.target else {
        return false;
    };
    let command = normalize_repeated_target_options(command.clone());
    let target_specs = [metadata.source, metadata.target];
    let arguments = command
        .arguments()
        .iter()
        .map_while(|argument| argument.as_string())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let option_prefix_len = short_option_prefix_len(command.name(), &arguments);
    let arguments = &arguments[..option_prefix_len];
    let mut independent_target = false;
    let mut explicit_source = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if let Some(spec) = short_flag_argument_parts(argument)
            .filter(|(_, attached)| attached.is_none())
            .and_then(|(flag, _)| target_specs.iter().flatten().find(|spec| spec.flag == flag))
        {
            let Some(value) = arguments.get(index + 1) else {
                return false;
            };
            if !target_is_independent(value, spec.find_type) {
                return false;
            }
            if spec.flag == target_spec.flag {
                independent_target = true;
            }
            if metadata
                .source
                .is_some_and(|source| source.flag == spec.flag)
            {
                explicit_source = true;
            }
        }
        index += 1 + usize::from(short_option_takes_next_value(command.name(), argument));
    }
    // A source-file wrapper without -t does not consume current itself. With
    // a live mark, defer inherited-current admission to the sourced commands,
    // just as the direct SourceFile entry does. Keep its target unresolved:
    // each child must re-read the live mark and reject stale current if needed.
    // Explicit relative targets already returned false above; a qualified -t
    // remains independent through the ordinary target check.
    (independent_target || (command.name() == "source-file" && marked_target.is_some()))
        && (explicit_source
            || !omitted_source_may_consume_current_target(
                command.name(),
                arguments,
                marked_target.is_some(),
            ))
}

pub(super) fn omitted_source_may_consume_current_target(
    command_name: &str,
    arguments: &[String],
    has_valid_marked_source: bool,
) -> bool {
    match command_name {
        "break-pane" | "link-window" => true,
        "join-pane" | "move-pane" | "swap-window" => !has_valid_marked_source,
        "move-window" => !has_bare_short_option(command_name, arguments, 'r'),
        "swap-pane" => {
            !['D', 'U']
                .into_iter()
                .any(|flag| has_bare_short_option(command_name, arguments, flag))
                && !has_valid_marked_source
        }
        _ => false,
    }
}

pub(super) fn has_bare_short_option(
    command_name: &str,
    arguments: &[String],
    expected: char,
) -> bool {
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if short_flag_argument_parts(argument)
            .is_some_and(|(flag, attached)| flag == expected && attached.is_none())
        {
            return true;
        }
        index += 1 + usize::from(short_option_takes_next_value(command_name, argument));
    }
    false
}

pub(super) fn target_is_independent(target: &str, find_type: TargetFindType) -> bool {
    match find_type {
        TargetFindType::Session => {
            !target.is_empty() && !matches!(target, "." | "" | "~" | "!" | "^" | "+" | "-")
        }
        TargetFindType::Window | TargetFindType::Pane => target
            .split_once(':')
            .is_some_and(|(session, _)| !session.is_empty()),
    }
}
