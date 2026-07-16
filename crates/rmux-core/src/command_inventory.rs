//! Shared command inventory and tmux-compatible `list-commands` rendering.

use std::borrow::Cow;
use std::fmt;

use crate::command_parser::COMMAND_TABLE;
use crate::formats::{render_template, FormatContext};

#[path = "command_inventory/signatures.rs"]
mod signatures;

use signatures::LIST_COMMAND_SIGNATURES;

const RMUX_EXTENSION_COMMANDS: &[&str] = &[
    "capabilities",
    "claude",
    "doctor",
    "setup",
    "wait-pane",
    "pane-snapshot",
    "stream-pane",
    "collect-pane-output",
    "locator",
    "expect-pane",
    "find-panes",
    "find-sessions",
    "broadcast-keys",
    "with-session",
    "web-share",
];

/// A typed command-name resolution failure from `list-commands`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListCommandsError {
    /// No public command or alias matched the requested name.
    Unknown(String),
    /// More than one public tmux command matched the requested prefix.
    Ambiguous(String),
}

impl fmt::Display for ListCommandsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(formatter, "unknown command: {name}"),
            Self::Ambiguous(name) => write!(formatter, "ambiguous command: {name}"),
        }
    }
}

impl std::error::Error for ListCommandsError {}

/// Renders the selected command inventory using tmux `list-commands` rules.
///
/// A bare listing omits RMUX-only extensions. An explicit lookup may select an
/// extension by its exact name, matching RMUX's direct CLI behavior.
pub fn render_list_commands(
    format: Option<&str>,
    requested_command: Option<&str>,
) -> Result<Vec<String>, ListCommandsError> {
    let requested = requested_command
        .map(resolve_list_commands_target)
        .transpose()?;
    Ok(LIST_COMMAND_SIGNATURES
        .iter()
        .copied()
        .filter(|(name, _)| match requested {
            None => !is_rmux_extension(name),
            Some(requested_name) => *name == requested_name,
        })
        .filter_map(|(name, _)| {
            let line = render_list_commands_line(format, name, list_command_alias(name));
            if format.is_some() && line.is_empty() {
                None
            } else {
                Some(line)
            }
        })
        .collect())
}

/// Renders one command inventory line with tmux command-list format fields.
#[must_use]
pub fn render_list_commands_line(format: Option<&str>, name: &str, alias: Option<&str>) -> String {
    let alias = alias.unwrap_or("");
    match format {
        Some(template) => {
            let usage = list_command_usage_without_alias(name);
            render_list_commands_template(template, name, alias, usage.as_ref())
        }
        None => format!("{name} {}", list_command_usage(name)),
    }
}

/// Iterates over every command name in inventory order, including RMUX extensions.
pub fn list_command_names() -> impl Iterator<Item = &'static str> {
    LIST_COMMAND_SIGNATURES.iter().map(|(name, _)| *name)
}

/// Returns whether a tmux command name or exact alias can resolve through the
/// frozen command table, including ambiguous command-name prefixes.
///
/// This is intentionally broader than [`crate::command_parser::lookup_command`]:
/// callers that need to preserve tmux's ambiguity diagnostic must still send
/// ambiguous prefixes through the command parser rather than treating them as
/// unknown extension aliases.
#[must_use]
pub fn has_tmux_command_candidate(name: &str) -> bool {
    COMMAND_TABLE
        .iter()
        .any(|entry| entry.alias == Some(name) || entry.name.starts_with(name))
}

fn resolve_list_commands_target(name: &str) -> Result<&'static str, ListCommandsError> {
    let name = list_commands_parser_alias(name);
    if let Some((command_name, _)) = LIST_COMMAND_SIGNATURES.iter().find(|(command_name, _)| {
        *command_name == name || list_command_alias(command_name) == Some(name)
    }) {
        return Ok(command_name);
    }

    let matches = LIST_COMMAND_SIGNATURES
        .iter()
        .map(|(command_name, _)| *command_name)
        .filter(|command_name| !is_rmux_extension(command_name))
        .filter(|command_name| {
            command_name.starts_with(name)
                || list_command_alias(command_name).is_some_and(|alias| alias.starts_with(name))
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [command] => Ok(command),
        [] => Err(ListCommandsError::Unknown(name.to_owned())),
        _ => Err(ListCommandsError::Ambiguous(name.to_owned())),
    }
}

fn list_commands_parser_alias(name: &str) -> &str {
    match name {
        "choose-session" | "choose-window" => "choose-tree",
        _ => name,
    }
}

fn list_command_alias(name: &str) -> Option<&'static str> {
    COMMAND_TABLE
        .iter()
        .find(|entry| entry.name == name)
        .and_then(|entry| entry.alias)
}

fn is_rmux_extension(name: &str) -> bool {
    RMUX_EXTENSION_COMMANDS.contains(&name)
}

fn render_list_commands_template(template: &str, name: &str, alias: &str, usage: &str) -> String {
    let variables = FormatContext::new()
        .with_named_value("command_list_name", name)
        .with_named_value("command_list_alias", alias)
        .with_named_value("command_list_usage", usage);
    render_template(template, &variables)
}

fn list_command_usage(name: &str) -> &'static str {
    LIST_COMMAND_SIGNATURES
        .iter()
        .find_map(|(command_name, usage)| (*command_name == name).then_some(*usage))
        .unwrap_or("")
}

fn list_command_usage_without_alias(name: &str) -> Cow<'static, str> {
    let usage = list_command_usage(name);
    if let Some(rest) = usage
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(") "))
    {
        Cow::Owned(rest.1.to_owned())
    } else {
        Cow::Borrowed(usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_follow_parser_table_then_rmux_extensions() {
        let names = list_command_names().collect::<Vec<_>>();
        let parser_names = COMMAND_TABLE
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(&names[..parser_names.len()], parser_names);
        assert_eq!(&names[parser_names.len()..], RMUX_EXTENSION_COMMANDS);
    }

    #[test]
    fn candidate_lookup_keeps_ambiguous_prefixes_and_exact_aliases() {
        assert!(has_tmux_command_candidate("list"));
        assert!(has_tmux_command_candidate("send"));
        assert!(has_tmux_command_candidate("send-keys"));
        assert!(!has_tmux_command_candidate("not-a-command"));
        assert!(!has_tmux_command_candidate("FOO=bar"));
    }

    #[test]
    fn formatted_list_commands_uses_command_list_fields_like_tmux() {
        let rendered = render_list_commands_line(
            Some(
                "#{command_name}|#{command_alias}|#{command_list_name}|#{command_list_alias}|#{command_list_usage}",
            ),
            "swap-window",
            Some("swapw"),
        );
        assert_eq!(
            rendered,
            "||swap-window|swapw|[-d] [-s src-window] [-t dst-window]"
        );
    }

    #[test]
    fn formatted_list_commands_preserves_tmux_escape_and_incomplete_rules() {
        assert_eq!(
            render_list_commands_line(
                Some("##{command_list_name}|abc#{|#{command_list_name}"),
                "link-window",
                Some("linkw"),
            ),
            "#{command_list_name}|abc"
        );
        assert_eq!(
            render_list_commands_line(
                Some("abc#{|#{command_list_name}|tail"),
                "link-window",
                Some("linkw"),
            ),
            "abc"
        );
    }

    #[test]
    fn formatted_list_commands_supports_tmux_conditionals_and_modifiers() {
        assert_eq!(
            render_list_commands_line(
                Some("#{?command_list_alias,alias,none}"),
                "list-commands",
                Some("lscm"),
            ),
            "alias"
        );
        assert_eq!(
            render_list_commands_line(
                Some("#{=5:command_list_name}"),
                "list-commands",
                Some("lscm"),
            ),
            "list-"
        );
        assert_eq!(
            render_list_commands_line(
                Some("#{?#{==:#{command_list_name},list-commands},#{command_list_alias},no}",),
                "list-commands",
                Some("lscm"),
            ),
            "lscm"
        );
    }

    #[test]
    fn explicit_lookup_resolves_aliases_extensions_and_errors() {
        assert_eq!(
            resolve_list_commands_target("neww").expect("neww resolves"),
            "new-window"
        );
        assert_eq!(
            resolve_list_commands_target("choose-session").expect("parser alias resolves"),
            "choose-tree"
        );
        assert_eq!(
            resolve_list_commands_target("web-share").expect("extension resolves"),
            "web-share"
        );
        assert_eq!(
            resolve_list_commands_target("nosuch"),
            Err(ListCommandsError::Unknown("nosuch".to_owned()))
        );
        assert_eq!(
            resolve_list_commands_target("list"),
            Err(ListCommandsError::Ambiguous("list".to_owned()))
        );
        assert_eq!(
            resolve_list_commands_target("wait-p"),
            Err(ListCommandsError::Unknown("wait-p".to_owned()))
        );
    }

    #[test]
    fn bare_listing_hides_extensions_while_explicit_lookup_keeps_them() {
        let bare = render_list_commands(Some("#{command_list_name}"), None)
            .expect("bare inventory renders");
        assert!(!bare.iter().any(|name| name == "web-share"));
        assert_eq!(
            render_list_commands(Some("#{command_list_name}"), Some("web-share"))
                .expect("explicit extension renders"),
            vec!["web-share"]
        );
    }
}
