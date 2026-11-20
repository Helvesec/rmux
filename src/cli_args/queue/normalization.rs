use super::*;

/// Applies tmux's `args_parse` order to one command's argv before clap sees it.
///
/// Every rejection here is one clap would also make; only the wording changes,
/// so the CLI prints tmux's `command NAME: ...` cause instead of clap's error
/// and usage block. A help or version request hands the line to clap
/// untouched, since clap answers it before reading the rest of the argv.
pub(crate) fn delimit_options_before_positionals(
    command_name: &'static str,
    command: &clap::Command,
    mut arguments: Vec<String>,
) -> Result<Vec<String>, clap::Error> {
    let short_flags = command
        .get_arguments()
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let value_flags = command
        .get_arguments()
        .filter(|argument| argument_requires_value(argument))
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let long_flags = command
        .get_arguments()
        .flat_map(long_names)
        .collect::<std::collections::BTreeSet<_>>();
    let long_value_flags = command
        .get_arguments()
        .filter(|argument| argument_requires_value(argument))
        .flat_map(long_names)
        .collect::<std::collections::BTreeSet<_>>();
    let optional_value_flags = command
        .get_arguments()
        .filter(|argument| argument_takes_optional_value(argument))
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let long_optional_value_flags = command
        .get_arguments()
        .filter(|argument| argument_takes_optional_value(argument))
        .flat_map(long_names)
        .collect::<std::collections::BTreeSet<_>>();
    let clap_answered_flags = command
        .get_arguments()
        .filter(|argument| is_help_or_version(argument))
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let long_clap_answered_flags = command
        .get_arguments()
        .filter(|argument| is_help_or_version(argument))
        .flat_map(long_names)
        .collect::<std::collections::BTreeSet<_>>();

    let mut expected_value_flag = None::<String>;
    let mut optional_value_pending = false;
    let mut first_positional = None;
    let mut options_terminated = false;
    for (index, argument) in arguments.iter().enumerate() {
        if expected_value_flag.take().is_some() {
            continue;
        }
        // Clap gives an RMUX optional value (`resize-pane -R 5`) the next
        // token unless it looks like a flag; it is not a positional then.
        if std::mem::take(&mut optional_value_pending) && is_positional_start(argument) {
            continue;
        }
        if argument == "--" {
            options_terminated = true;
            first_positional = (index + 1 < arguments.len()).then_some(index + 1);
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            first_positional = Some(index);
            break;
        }
        if let Some(long) = argument.strip_prefix("--") {
            let name = long.split_once('=').map_or(long, |(name, _)| name);
            if !long_flags.contains(name) {
                // tmux reads `--name` as the flag '-' in a cluster.
                return Err(unknown_flag_error(command_name, "--".to_owned()));
            }
            if long_clap_answered_flags.contains(name) {
                return Ok(arguments);
            }
            if !long.contains('=') {
                if long_value_flags.contains(name) {
                    expected_value_flag = Some(format!("--{name}"));
                }
                optional_value_pending = long_optional_value_flags.contains(name);
            }
            continue;
        }

        let mut chars = argument[1..].chars().peekable();
        while let Some(flag) = chars.next() {
            if flag == '?' {
                return Err(usage_error(command_name));
            }
            if !short_flags.contains(&flag) {
                return Err(unknown_flag_error(command_name, format!("-{flag}")));
            }
            if clap_answered_flags.contains(&flag) {
                return Ok(arguments);
            }
            if value_flags.contains(&flag) {
                if chars.peek().is_none() {
                    expected_value_flag = Some(format!("-{flag}"));
                }
                break;
            }
            if optional_value_flags.contains(&flag) {
                optional_value_pending = chars.peek().is_none();
                break;
            }
        }
    }

    if let Some(flag) = expected_value_flag {
        return Err(missing_value_error(command_name, flag));
    }

    if let Some(index) = first_positional {
        let positional_limit = command
            .get_positionals()
            .map(|argument| {
                argument
                    .get_num_args()
                    .map_or(1, |range| range.max_values())
            })
            .fold(0_usize, usize::saturating_add);
        if arguments.len() - index > positional_limit {
            return Err(too_many_arguments_error(command_name, positional_limit));
        }

        // tmux stops option parsing at the first positional. Clap otherwise
        // accepts recognized flags later in the argv and can silently change
        // a command's target or scope instead of rejecting the extra tail.
        if !options_terminated {
            arguments.insert(index, "--".to_owned());
        }
    }

    Ok(arguments)
}

/// Every long spelling clap accepts for `argument`, aliases included.
fn long_names(argument: &clap::Arg) -> Vec<&str> {
    argument
        .get_long()
        .into_iter()
        .chain(argument.get_all_aliases().unwrap_or_default())
        .collect()
}

/// Clap prints help or the version as soon as it reads this flag, whatever
/// follows it, so the scan must not reject the tokens after it.
fn is_help_or_version(argument: &clap::Arg) -> bool {
    matches!(
        argument.get_action(),
        ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
    )
}

pub(crate) fn argument_requires_value(argument: &clap::Arg) -> bool {
    matches!(argument.get_action(), ArgAction::Set | ArgAction::Append)
        && argument
            .get_num_args()
            .is_none_or(|range| range.min_values() > 0)
}

fn argument_takes_optional_value(argument: &clap::Arg) -> bool {
    matches!(argument.get_action(), ArgAction::Set | ArgAction::Append)
        && argument
            .get_num_args()
            .is_some_and(|range| range.min_values() == 0 && range.max_values() > 0)
}

pub(crate) fn unknown_flag_error(command_name: &'static str, flag: String) -> clap::Error {
    // tmux's args_parse calls a non-alphanumeric flag such as ':' or '-'
    // invalid, and any other flag missing from the template unknown.
    let alphanumeric = flag
        .strip_prefix('-')
        .and_then(|flag| flag.chars().next())
        .is_some_and(|flag| flag.is_ascii_alphanumeric());
    let diagnostic = if alphanumeric { "unknown" } else { "invalid" };
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("command {command_name}: {diagnostic} flag {flag}"),
    )
}

/// tmux answers `-?` with the command's usage line instead of a cause.
fn usage_error(command_name: &'static str) -> clap::Error {
    let usage = rmux_core::command_inventory::render_list_commands_line(
        Some("#{command_list_usage}"),
        command_name,
        None,
    );
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("usage: {command_name} {usage}"),
    )
}

pub(crate) fn too_many_arguments_error(command_name: &'static str, limit: usize) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::TooManyValues,
        format!("command {command_name}: too many arguments (need at most {limit})"),
    )
}

pub(crate) fn missing_value_error(command_name: &'static str, flag: String) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::ValueValidation,
        format!("command {command_name}: {flag} expects an argument"),
    )
}

pub(crate) fn normalize_attached_short_values(
    command: &clap::Command,
    arguments: Vec<String>,
) -> Vec<String> {
    let boolean_flags = command
        .get_arguments()
        .filter(|argument| {
            matches!(
                argument.get_action(),
                ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
            )
        })
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let value_flags = command
        .get_arguments()
        .filter(|argument| argument_requires_value(argument))
        .filter_map(clap::Arg::get_short)
        .collect::<std::collections::BTreeSet<_>>();
    let repeat_last_wins_flags = if command.get_name() == "new-window" {
        command
            .get_arguments()
            .filter(|argument| matches!(argument.get_action(), ArgAction::Set))
            .filter_map(clap::Arg::get_short)
            .filter(|flag| *flag == 't')
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    if value_flags.is_empty() {
        return arguments;
    }
    let long_value_flags = command
        .get_arguments()
        .filter(|argument| argument_requires_value(argument))
        .flat_map(|argument| {
            argument
                .get_long()
                .into_iter()
                .chain(argument.get_all_aliases().unwrap_or_default())
        })
        .collect::<std::collections::BTreeSet<_>>();

    let mut normalized_options = Vec::with_capacity(arguments.len());
    let mut trailing_values = Vec::new();
    let mut passthrough = false;
    let mut expect_next_value = false;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if passthrough {
            trailing_values.push(argument);
            continue;
        }
        if expect_next_value {
            normalized_options.push(argument);
            expect_next_value = false;
            continue;
        }
        if argument == "--" {
            passthrough = true;
            trailing_values.push(argument);
            continue;
        }

        if is_positional_start(&argument) {
            trailing_values.push(argument);
            trailing_values.extend(arguments);
            break;
        }

        if let Some((parts, needs_next_value)) =
            normalize_compact_short_value_token(&argument, &boolean_flags, &value_flags)
        {
            normalized_options.extend(parts);
            expect_next_value = needs_next_value;
        } else if exact_short_flag(&argument, &value_flags).is_some()
            || argument
                .strip_prefix("--")
                .is_some_and(|flag| long_value_flags.contains(flag))
        {
            // Long options and their aliases own the next token just like
            // short options, before compact short flags can be expanded.
            expect_next_value = true;
            normalized_options.push(argument);
        } else {
            normalized_options.push(argument);
        }
    }

    let mut normalized = collapse_repeated_short_values(
        normalized_options,
        &repeat_last_wins_flags,
        &value_flags,
        &long_value_flags,
    );
    normalized.extend(trailing_values);
    normalized
}

pub(crate) fn is_positional_start(argument: &str) -> bool {
    !argument.starts_with('-') || argument == "-"
}

pub(crate) fn normalize_compact_short_value_token(
    argument: &str,
    boolean_flags: &std::collections::BTreeSet<char>,
    value_flags: &std::collections::BTreeSet<char>,
) -> Option<(Vec<String>, bool)> {
    let flags = argument.strip_prefix('-')?;
    if flags.is_empty() || flags.starts_with('-') {
        return None;
    }

    let mut chars = flags.char_indices().peekable();
    let first = chars.next()?;
    chars.peek()?;

    let mut parts = Vec::new();
    for (index, flag) in std::iter::once(first).chain(chars.by_ref()) {
        if boolean_flags.contains(&flag) {
            parts.push(format!("-{flag}"));
            continue;
        }
        if value_flags.contains(&flag) {
            parts.push(format!("-{flag}"));
            let value_start = index + flag.len_utf8();
            if value_start < flags.len() {
                parts.push(flags[value_start..].to_owned());
                return Some((parts, false));
            }
            return Some((parts, true));
        }
        return None;
    }

    Some((parts, false))
}

pub(crate) fn collapse_repeated_short_values(
    arguments: Vec<String>,
    repeat_last_wins_flags: &std::collections::BTreeSet<char>,
    value_flags: &std::collections::BTreeSet<char>,
    long_value_flags: &std::collections::BTreeSet<&str>,
) -> Vec<String> {
    if repeat_last_wins_flags.is_empty() {
        return arguments;
    }

    let mut occurrences = std::collections::BTreeMap::<char, Vec<(usize, usize)>>::new();
    let mut index = 0;
    while index + 1 < arguments.len() {
        // Compact short values are already split. Every value-taking option
        // owns its next token, even when that token looks like a repeated flag.
        if let Some(flag) = exact_short_flag(&arguments[index], value_flags) {
            if repeat_last_wins_flags.contains(&flag) {
                occurrences
                    .entry(flag)
                    .or_default()
                    .push((index, index + 1));
            }
            index += 2;
        } else if arguments[index]
            .strip_prefix("--")
            .is_some_and(|flag| long_value_flags.contains(flag))
        {
            index += 2;
        } else {
            index += 1;
        }
    }

    let mut drop_indexes = std::collections::BTreeSet::new();
    for positions in occurrences.values() {
        for (flag_index, value_index) in positions
            .iter()
            .take(positions.len().saturating_sub(1))
            .copied()
        {
            drop_indexes.insert(flag_index);
            drop_indexes.insert(value_index);
        }
    }

    arguments
        .into_iter()
        .enumerate()
        .filter_map(|(index, argument)| (!drop_indexes.contains(&index)).then_some(argument))
        .collect()
}

pub(crate) fn exact_short_flag(
    argument: &str,
    flags: &std::collections::BTreeSet<char>,
) -> Option<char> {
    let mut chars = argument.chars();
    if chars.next()? != '-' || chars.as_str().starts_with('-') {
        return None;
    }
    let flag = chars.next()?;
    (chars.next().is_none() && flags.contains(&flag)).then_some(flag)
}
