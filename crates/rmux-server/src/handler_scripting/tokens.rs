use std::collections::VecDeque;

use rmux_core::{
    command_inventory::{command_short_option_spec, CommandShortOptionSpec},
    command_parser::{CommandArgument, ParsedCommand},
    command_target_metadata,
};
use rmux_proto::RmuxError;

/// Applies tmux's last-value precedence before queued target lookup.
///
/// Superseded source and target values must be removed before any parser can
/// resolve or validate them; only the final value for each flag is meaningful.
pub(super) fn normalize_repeated_target_options(command: ParsedCommand) -> ParsedCommand {
    let Some(metadata) = command_target_metadata(command.name()) else {
        return command;
    };
    let target_flags = [metadata.source, metadata.target]
        .into_iter()
        .flatten()
        .map(|spec| spec.flag)
        .collect::<Vec<_>>();
    let scalar_arguments = command
        .arguments()
        .iter()
        .map_while(CommandArgument::as_string)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let option_prefix_len = short_option_prefix_len(command.name(), &scalar_arguments);
    if option_prefix_len == 0 {
        return command;
    }

    let option_prefix = normalize_compact_short_options(
        command.name(),
        scalar_arguments[..option_prefix_len].to_vec(),
    );
    let option_prefix = retain_last_target_options(command.name(), option_prefix, &target_flags);
    let mut arguments = option_prefix
        .into_iter()
        .map(CommandArgument::String)
        .collect::<Vec<_>>();
    arguments.extend(command.arguments()[option_prefix_len..].iter().cloned());
    command.with_arguments(arguments)
}

fn retain_last_target_options(
    command_name: &str,
    arguments: Vec<String>,
    target_flags: &[char],
) -> Vec<String> {
    let Some(spec) = command_short_option_spec(command_name) else {
        return arguments;
    };
    let mut last_occurrences = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if let Some(flag) = separated_short_flag(&arguments[index]) {
            if target_flags.contains(&flag) {
                last_occurrences.retain(|(candidate, _)| *candidate != flag);
                last_occurrences.push((flag, index));
            }
            index += usize::from(spec.takes_value(flag));
        }
        index += 1;
    }

    let mut retained = Vec::with_capacity(arguments.len());
    let mut index = 0;
    while index < arguments.len() {
        if let Some(flag) = separated_short_flag(&arguments[index]) {
            let takes_value = spec.takes_value(flag);
            if !target_flags.contains(&flag)
                || last_occurrences
                    .iter()
                    .any(|(candidate, last_index)| *candidate == flag && *last_index == index)
            {
                retained.push(arguments[index].clone());
                if takes_value {
                    if let Some(value) = arguments.get(index + 1) {
                        retained.push(value.clone());
                    }
                }
            }
            index += usize::from(takes_value);
        } else {
            retained.push(arguments[index].clone());
        }
        index += 1;
    }
    retained
}

fn separated_short_flag(argument: &str) -> Option<char> {
    let mut characters = argument.strip_prefix('-')?.chars();
    let flag = characters.next()?;
    characters.next().is_none().then_some(flag)
}

pub(super) fn normalize_compact_short_options(
    command_name: &str,
    arguments: Vec<String>,
) -> Vec<String> {
    let Some(spec) = command_short_option_spec(command_name) else {
        return arguments;
    };
    let option_prefix_len = short_option_prefix_len_with_spec(&arguments, spec);

    let mut normalized = Vec::with_capacity(arguments.len());
    let mut expects_value = false;

    for (index, argument) in arguments.into_iter().enumerate() {
        if index >= option_prefix_len {
            normalized.push(argument);
            continue;
        }
        if expects_value {
            normalized.push(argument);
            expects_value = false;
            continue;
        }
        let mut short_flags = argument[1..].chars();
        if let (Some(flag), None) = (short_flags.next(), short_flags.next()) {
            expects_value = spec.takes_value(flag);
            normalized.push(argument);
            continue;
        }

        let Some((parts, needs_next_value)) = normalize_short_option_token(&argument, spec) else {
            normalized.push(argument);
            continue;
        };
        normalized.extend(parts);
        expects_value = needs_next_value;
    }

    normalized
}

pub(super) fn short_option_prefix_len(command_name: &str, arguments: &[String]) -> usize {
    command_short_option_spec(command_name)
        .map_or(0, |spec| short_option_prefix_len_with_spec(arguments, spec))
}

pub(super) fn short_option_takes_next_value(command_name: &str, argument: &str) -> bool {
    command_short_option_spec(command_name)
        .is_some_and(|spec| short_option_token_needs_next_value(argument, spec) == Some(true))
}

fn short_option_prefix_len_with_spec(arguments: &[String], spec: &CommandShortOptionSpec) -> usize {
    let mut expects_value = false;

    for (index, argument) in arguments.iter().enumerate() {
        if expects_value {
            expects_value = false;
            continue;
        }
        if argument == "--"
            || argument == "-"
            || !argument.starts_with('-')
            || argument.starts_with("--")
        {
            return index;
        }
        let Some(needs_next_value) = short_option_token_needs_next_value(argument, spec) else {
            return index;
        };
        expects_value = needs_next_value;
    }

    arguments.len()
}

fn short_option_token_needs_next_value(
    argument: &str,
    spec: &CommandShortOptionSpec,
) -> Option<bool> {
    let flags = argument.strip_prefix('-')?;
    if flags.is_empty() || flags.starts_with('-') {
        return None;
    }

    for (index, flag) in flags.char_indices() {
        if spec.takes_value(flag) {
            return Some(index + flag.len_utf8() == flags.len());
        }
        if !spec.is_boolean(flag) {
            return None;
        }
    }

    Some(false)
}

fn normalize_short_option_token(
    argument: &str,
    spec: &CommandShortOptionSpec,
) -> Option<(Vec<String>, bool)> {
    let flags = argument.strip_prefix('-')?;
    if flags.is_empty() || flags.starts_with('-') {
        return None;
    }

    let mut parts = Vec::new();
    let mut seen_boolean_flags = Vec::new();
    let mut chars = flags.char_indices().peekable();
    while let Some((_, flag)) = chars.next() {
        if spec.takes_value(flag) {
            parts.push(format!("-{flag}"));
            let value_start = chars.peek().map_or(flags.len(), |(index, _)| *index);
            if value_start < flags.len() {
                parts.push(flags[value_start..].to_owned());
                return Some((parts, false));
            }
            return Some((parts, true));
        }
        if spec.is_boolean(flag) {
            if !seen_boolean_flags.contains(&flag) {
                seen_boolean_flags.push(flag);
                parts.push(format!("-{flag}"));
            }
            continue;
        }
        return None;
    }

    Some((parts, false))
}

pub(super) fn rebuild_shell_command(command_parts: Vec<String>) -> String {
    if command_parts.len() == 1 {
        return command_parts
            .into_iter()
            .next()
            .expect("single shell token");
    }

    command_parts
        .into_iter()
        .map(shell_command_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_command_token(token: String) -> String {
    format!("'{}'", token.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CompactFlag {
    Bare(char),
    Value { flag: char, value: Option<String> },
}

impl CompactFlag {
    pub(super) fn value_or_next(
        self,
        args: &mut CommandTokens,
        description: &str,
    ) -> Result<String, RmuxError> {
        match self {
            Self::Value {
                value: Some(value), ..
            } => Ok(value),
            Self::Value { value: None, .. } => args.required(description),
            Self::Bare(flag) => Err(RmuxError::Server(format!(
                "flag -{flag} does not take {description}"
            ))),
        }
    }
}

pub(super) fn parse_compact_flag_cluster(
    token: &str,
    bare_flags: &str,
    value_flags: &str,
) -> Option<Vec<CompactFlag>> {
    if !token.starts_with('-') || token == "-" || token == "--" || token.len() <= 2 {
        return None;
    }

    let flags = token.strip_prefix('-')?;
    let mut cluster = Vec::new();
    for (index, flag) in flags.char_indices() {
        if bare_flags.contains(flag) {
            cluster.push(CompactFlag::Bare(flag));
            continue;
        }
        if value_flags.contains(flag) {
            let value_start = index + flag.len_utf8();
            let value = (value_start < flags.len()).then(|| flags[value_start..].to_owned());
            cluster.push(CompactFlag::Value { flag, value });
            return Some(cluster);
        }
        return None;
    }

    Some(cluster)
}

pub(super) struct CommandTokens {
    tokens: VecDeque<String>,
}

impl CommandTokens {
    pub(super) fn new(tokens: Vec<String>) -> Self {
        Self {
            tokens: tokens.into_iter().collect(),
        }
    }

    pub(super) fn required(&mut self, description: &str) -> Result<String, RmuxError> {
        self.tokens
            .pop_front()
            .ok_or_else(|| RmuxError::Server(format!("missing {description}")))
    }

    pub(super) fn optional(&mut self) -> Option<String> {
        self.tokens.pop_front()
    }

    pub(super) fn peek(&self) -> Option<&str> {
        self.tokens.front().map(String::as_str)
    }

    pub(super) fn optional_compact_flags(&mut self, allowed: &str) -> Option<Vec<char>> {
        let token = self.peek()?;
        if !token.starts_with('-') || token == "-" || token == "--" || token.len() <= 2 {
            return None;
        }
        let flags = token.strip_prefix('-')?;
        if !flags.chars().all(|flag| allowed.contains(flag)) {
            return None;
        }
        let token = self.optional().expect("peeked flag token must exist");
        Some(
            token
                .strip_prefix('-')
                .expect("validated compact flag token")
                .chars()
                .collect(),
        )
    }

    pub(super) fn peek_is_flag(&self) -> bool {
        self.tokens
            .front()
            .is_some_and(|token| token.starts_with('-') && token != "-")
    }

    pub(super) fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub(super) fn remaining(self) -> Vec<String> {
        self.tokens.into_iter().collect()
    }

    pub(super) fn remaining_joined(self) -> String {
        self.tokens.into_iter().collect::<Vec<_>>().join(" ")
    }

    pub(super) fn no_extra(&self, command: &str) -> Result<(), RmuxError> {
        if let Some(extra) = self.tokens.front() {
            return Err(RmuxError::Server(format!(
                "unexpected argument '{extra}' for {command}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::command_parser::{CommandArgument, CommandParser};

    use super::{
        normalize_compact_short_options, normalize_repeated_target_options, short_option_prefix_len,
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn normalized_arguments(command: &str) -> Vec<CommandArgument> {
        let parsed = CommandParser::new().parse(command).expect("command parses");
        assert_eq!(parsed.commands().len(), 1);
        normalize_repeated_target_options(parsed.commands()[0].clone())
            .arguments()
            .to_vec()
    }

    fn string_arguments(values: &[&str]) -> Vec<CommandArgument> {
        values
            .iter()
            .map(|value| CommandArgument::String((*value).to_owned()))
            .collect()
    }

    #[test]
    fn compact_short_options_stop_at_value_and_positional_boundaries() {
        assert_eq!(
            normalize_compact_short_options(
                "run-shell",
                args(&["-Ctalpha:0.0", "display-message -p ok", "-JT"]),
            ),
            args(&["-C", "-t", "alpha:0.0", "display-message -p ok", "-JT",])
        );
        assert_eq!(
            normalize_compact_short_options(
                "run-shell",
                args(&["-c", "-hyphenated-directory", "printf", "-JT"]),
            ),
            args(&["-c", "-hyphenated-directory", "printf", "-JT"])
        );
        assert_eq!(
            normalize_compact_short_options("run-shell", args(&["--", "-JT"])),
            args(&["--", "-JT"])
        );
        assert_eq!(
            short_option_prefix_len("set-option", &args(&["-g", "@compact", "-tfoo"])),
            1
        );
        assert_eq!(
            short_option_prefix_len(
                "list-windows",
                &args(&["-F", "-tfoo", "-t", "alpha", "message"]),
            ),
            4
        );
    }

    #[test]
    fn compact_short_options_leave_unknown_or_optional_value_forms_to_command_parser() {
        assert_eq!(
            normalize_compact_short_options("show-messages", args(&["-JX"])),
            args(&["-JX"])
        );
        assert_eq!(
            normalize_compact_short_options("run-shell", args(&["-x", "-bE"])),
            args(&["-x", "-bE"])
        );
        assert_eq!(
            normalize_compact_short_options("resize-pane", args(&["-D5", "-Z"])),
            args(&["-D5", "-Z"])
        );
    }

    #[test]
    fn compact_short_options_include_hidden_tmux_flags_without_public_inventory_churn() {
        assert_eq!(
            normalize_compact_short_options("join-pane", args(&["-p35", "-s", "%1", "-t", "%0"]),),
            args(&["-p", "35", "-s", "%1", "-t", "%0"])
        );
        assert_eq!(
            normalize_compact_short_options("move-pane", args(&["-p35", "-s", "%1", "-t", "%0"]),),
            args(&["-p", "35", "-s", "%1", "-t", "%0"])
        );
        assert_eq!(
            normalize_compact_short_options("load-buffer", args(&["-wbclip", "/tmp/input"])),
            args(&["-w", "-b", "clip", "/tmp/input"])
        );
        assert_eq!(
            normalize_compact_short_options("list-buffers", args(&["-rF#{buffer_name}"])),
            args(&["-r", "-F", "#{buffer_name}"])
        );
    }

    #[test]
    fn repeated_boolean_cluster_is_deduplicated_before_expansion() {
        let repeated = format!("-{}", "JT".repeat(512 * 1024));

        assert_eq!(
            normalize_compact_short_options("show-messages", vec![repeated]),
            args(&["-J", "-T"])
        );
    }

    #[test]
    fn repeated_target_options_keep_only_the_last_value_for_each_target_flag() {
        assert_eq!(
            normalized_arguments("swap-window -smissing -salpha:0 -t bad:xyz.??? -tbeta:0 -d"),
            string_arguments(&["-s", "alpha:0", "-t", "beta:0", "-d"])
        );
        assert_eq!(
            normalized_arguments("new-window -t bad:xyz.??? -t alpha -n chosen"),
            string_arguments(&["-t", "alpha", "-n", "chosen"])
        );
    }

    #[test]
    fn repeated_target_normalization_respects_option_and_positional_boundaries() {
        assert_eq!(
            normalized_arguments("display-message -F -t -t missing -t alpha:0 message -t tail"),
            string_arguments(&["-F", "-t", "-t", "alpha:0", "message", "-t", "tail"])
        );
    }

    #[test]
    fn repeated_target_normalization_preserves_nested_command_arguments() {
        let normalized =
            normalized_arguments("if-shell -t missing -t alpha:0.0 -F 1 { display-message -p ok }");
        assert_eq!(
            &normalized[..4],
            string_arguments(&["-t", "alpha:0.0", "-F", "1"])
        );
        assert!(matches!(
            normalized.last(),
            Some(CommandArgument::Commands(_))
        ));
    }
}
