//! Shared command inventory and tmux-compatible `list-commands` rendering.

use std::borrow::Cow;
use std::fmt;
use std::sync::OnceLock;

use crate::command_parser::{CommandEntry, COMMAND_TABLE};
use crate::formats::{render_template, FormatContext};

#[path = "command_inventory/signatures.rs"]
mod signatures;

use signatures::LIST_COMMAND_SIGNATURES;

/// Typed short-option metadata used by internal command normalization.
///
/// Public options are derived from the frozen `list-commands` signature. A
/// small internal supplement carries implemented tmux options that tmux itself
/// intentionally omits from `list-commands`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandShortOptionSpec {
    boolean_flags: String,
    value_flags: String,
}

impl CommandShortOptionSpec {
    fn from_usage(usage: &str) -> Self {
        let mut spec = Self::default();
        let mut remaining = usage;

        while let Some(group_start) = remaining.find('[') {
            let after_start = &remaining[group_start + 1..];
            let Some(group_end) = after_start.find(']') else {
                break;
            };
            spec.add_usage_group(&after_start[..group_end]);
            remaining = &after_start[group_end + 1..];
        }

        spec
    }

    /// Returns whether `flag` is a boolean short option for the command.
    #[must_use]
    pub fn is_boolean(&self, flag: char) -> bool {
        self.boolean_flags.contains(flag)
    }

    /// Returns whether `flag` consumes a value for the command.
    #[must_use]
    pub fn takes_value(&self, flag: char) -> bool {
        self.value_flags.contains(flag)
    }

    fn add_boolean(&mut self, flag: char) {
        if flag.is_ascii() && !self.value_flags.contains(flag) && !self.boolean_flags.contains(flag)
        {
            self.boolean_flags.push(flag);
        }
    }

    fn add_value(&mut self, flag: char) {
        if flag.is_ascii() && !self.value_flags.contains(flag) {
            // tmux 3.7b advertises split-window's -e in both the compact
            // boolean group and the explicit `-e environment` group.  The
            // value-taking spelling is the only safe boundary for compact
            // normalization, so an explicit value group wins.
            self.boolean_flags.retain(|candidate| candidate != flag);
            self.value_flags.push(flag);
        }
    }

    fn add_usage_group(&mut self, group: &str) {
        let group = group.trim();
        if !group.starts_with('-') || group.starts_with("--") {
            return;
        }

        let alternatives = group.split('|').collect::<Vec<_>>();
        if alternatives.len() > 1
            && alternatives
                .iter()
                .all(|alternative| single_short_flag(alternative.trim()).is_some())
        {
            for alternative in alternatives {
                self.add_boolean(
                    single_short_flag(alternative.trim())
                        .expect("validated short-option alternative"),
                );
            }
            return;
        }

        let mut words = group.split_ascii_whitespace();
        let Some(option) = words.next() else {
            return;
        };
        let Some(flags) = option.strip_prefix('-') else {
            return;
        };
        if flags.is_empty() || flags.starts_with('-') {
            return;
        }

        if words.next().is_some() {
            if let Some(flag) = single_short_flag(option) {
                self.add_value(flag);
            }
            return;
        }

        for flag in flags.chars() {
            self.add_boolean(flag);
        }
    }

    fn add_internal_boolean_flags(&mut self, flags: &str) {
        for flag in flags.chars() {
            self.add_boolean(flag);
        }
    }

    fn add_internal_value_flags(&mut self, flags: &str) {
        for flag in flags.chars() {
            self.add_value(flag);
        }
    }
}

fn add_internal_short_options(command_name: &str, spec: &mut CommandShortOptionSpec) {
    // tmux 3.7b accepts these flags but does not render them in
    // `list-commands`. Keep normalization metadata separate so the public
    // inventory remains oracle-exact.
    match command_name {
        "join-pane" | "move-pane" => spec.add_internal_value_flags("p"),
        "list-buffers" => spec.add_internal_boolean_flags("r"),
        "load-buffer" => spec.add_internal_boolean_flags("w"),
        _ => {}
    }
}

/// Returns typed short-option metadata for a public command.
///
/// Metadata is derived from the same frozen signature rendered by
/// `list-commands`, including adjacent option groups and tmux's `-L|-S|-U`
/// spelling for mutually exclusive boolean flags.
#[must_use]
pub fn command_short_option_spec(command_name: &str) -> Option<&'static CommandShortOptionSpec> {
    static SPECS: OnceLock<Vec<(&'static str, CommandShortOptionSpec)>> = OnceLock::new();
    SPECS
        .get_or_init(|| {
            LIST_COMMAND_SIGNATURES
                .iter()
                .map(|(name, usage)| {
                    let mut spec = CommandShortOptionSpec::from_usage(usage);
                    add_internal_short_options(name, &mut spec);
                    (*name, spec)
                })
                .collect()
        })
        .iter()
        .find_map(|(name, spec)| (*name == command_name).then_some(spec))
}

fn single_short_flag(option: &str) -> Option<char> {
    let mut flags = option.strip_prefix('-')?.chars();
    let flag = flags.next()?;
    (flags.next().is_none() && flag.is_ascii()).then_some(flag)
}

/// Exact-only RMUX command extensions shared by client parsing and internal
/// server canonicalization. They never participate in tmux prefix matching.
pub const RMUX_EXTENSION_COMMANDS: &[CommandEntry] = &[
    CommandEntry {
        name: "capabilities",
        alias: None,
    },
    CommandEntry {
        name: "claude",
        alias: None,
    },
    CommandEntry {
        name: "doctor",
        alias: None,
    },
    CommandEntry {
        name: "setup",
        alias: None,
    },
    CommandEntry {
        name: "wait-pane",
        alias: None,
    },
    CommandEntry {
        name: "pane-snapshot",
        alias: None,
    },
    CommandEntry {
        name: "stream-pane",
        alias: None,
    },
    CommandEntry {
        name: "collect-pane-output",
        alias: None,
    },
    CommandEntry {
        name: "locator",
        alias: None,
    },
    CommandEntry {
        name: "expect-pane",
        alias: None,
    },
    CommandEntry {
        name: "find-panes",
        alias: None,
    },
    CommandEntry {
        name: "find-sessions",
        alias: None,
    },
    CommandEntry {
        name: "broadcast-keys",
        alias: None,
    },
    CommandEntry {
        name: "with-session",
        alias: None,
    },
    CommandEntry {
        name: "web-share",
        alias: None,
    },
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
    render_list_commands_with_optional_socket_path(format, requested_command, None)
}

/// Renders the selected command inventory with the selected server socket available to formats.
pub fn render_list_commands_for_socket(
    format: Option<&str>,
    requested_command: Option<&str>,
    socket_path: &str,
) -> Result<Vec<String>, ListCommandsError> {
    render_list_commands_with_optional_socket_path(format, requested_command, Some(socket_path))
}

fn render_list_commands_with_optional_socket_path(
    format: Option<&str>,
    requested_command: Option<&str>,
    socket_path: Option<&str>,
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
            let line = render_list_commands_line_with_optional_socket_path(
                format,
                name,
                list_command_alias(name),
                socket_path,
            );
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
    render_list_commands_line_with_optional_socket_path(format, name, alias, None)
}

fn render_list_commands_line_with_optional_socket_path(
    format: Option<&str>,
    name: &str,
    alias: Option<&str>,
    socket_path: Option<&str>,
) -> String {
    let alias = alias.unwrap_or("");
    match format {
        Some(template) => {
            let usage = list_command_usage_without_alias(name);
            render_list_commands_template(template, name, alias, usage.as_ref(), socket_path)
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
    RMUX_EXTENSION_COMMANDS
        .iter()
        .any(|entry| entry.name == name)
}

fn render_list_commands_template(
    template: &str,
    name: &str,
    alias: &str,
    usage: &str,
    socket_path: Option<&str>,
) -> String {
    let mut variables = FormatContext::new()
        .with_named_value("command_list_name", name)
        .with_named_value("command_list_alias", alias)
        .with_named_value("command_list_usage", usage);
    if let Some(socket_path) = socket_path {
        variables = variables.with_named_value("socket_path", socket_path);
    }
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
        assert_eq!(
            &names[parser_names.len()..],
            RMUX_EXTENSION_COMMANDS
                .iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn short_option_specs_derive_boolean_and_value_boundaries_from_inventory() {
        let capture = command_short_option_spec("capture-pane").expect("capture-pane signature");
        for flag in ['a', 'e', 'J', 'p', 'q'] {
            assert!(capture.is_boolean(flag), "capture-pane -{flag}");
        }
        for flag in ['b', 'E', 'S', 't'] {
            assert!(capture.takes_value(flag), "capture-pane -{flag}");
        }

        let run_shell = command_short_option_spec("run-shell").expect("run-shell signature");
        for flag in ['b', 'C', 'E'] {
            assert!(run_shell.is_boolean(flag), "run-shell -{flag}");
        }
        for flag in ['c', 'd', 't'] {
            assert!(run_shell.takes_value(flag), "run-shell -{flag}");
        }

        let wait_for = command_short_option_spec("wait-for").expect("wait-for signature");
        for flag in ['L', 'S', 'U'] {
            assert!(wait_for.is_boolean(flag), "wait-for -{flag}");
        }

        let list_clients =
            command_short_option_spec("list-clients").expect("list-clients signature");
        assert!(list_clients.takes_value('O'));
        assert!(list_clients.takes_value('t'));

        let split_window =
            command_short_option_spec("split-window").expect("split-window signature");
        assert!(split_window.is_boolean('k'));
        assert!(split_window.takes_value('e'));
        assert!(split_window.takes_value('l'));
        assert!(split_window.takes_value('p'));
        assert!(!split_window.is_boolean('e'));
        assert!(!split_window.is_boolean('p'));

        let load_buffer = command_short_option_spec("load-buffer").expect("load-buffer signature");
        assert!(load_buffer.is_boolean('w'));
        assert!(load_buffer.takes_value('b'));

        let list_buffers =
            command_short_option_spec("list-buffers").expect("list-buffers signature");
        assert!(list_buffers.is_boolean('r'));
        assert!(command_short_option_spec("not-a-command").is_none());
    }

    #[test]
    fn internal_short_options_do_not_change_public_signatures() {
        let load_buffer = LIST_COMMAND_SIGNATURES
            .iter()
            .find_map(|(name, usage)| (*name == "load-buffer").then_some(*usage))
            .expect("load-buffer public signature");
        let list_buffers = LIST_COMMAND_SIGNATURES
            .iter()
            .find_map(|(name, usage)| (*name == "list-buffers").then_some(*usage))
            .expect("list-buffers public signature");

        assert!(!load_buffer.contains("-w"));
        assert!(!list_buffers.contains("-r"));
    }

    #[test]
    fn short_option_inventory_has_unambiguous_value_boundaries() {
        for (command_name, _) in LIST_COMMAND_SIGNATURES {
            let spec = command_short_option_spec(command_name).expect("inventory command spec");
            for flag in spec.boolean_flags.chars() {
                assert!(
                    !spec.value_flags.contains(flag),
                    "{command_name} -{flag} is advertised as both boolean and value-taking"
                );
            }
        }
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
