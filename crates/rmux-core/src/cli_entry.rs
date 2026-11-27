//! Entry decisions shared by the full CLI and the daemon's execution offer.

/// Whether already-canonical command argv requires explicit split/key admission.
/// Values and the process command tail are not interpreted as split flags.
#[must_use]
pub fn split_command_requires_key_admission(
    command: &crate::command_parser::ParsedCommand,
) -> bool {
    if command.name() != "split-window" {
        return false;
    }
    let spec = crate::command_inventory::command_short_option_spec("split-window")
        .expect("split-window has a short-option inventory");
    let mut args = command.arguments().iter();
    while let Some(argument) = args.next().and_then(|arg| arg.as_string()) {
        let Some(flags) = argument.strip_prefix('-').filter(|flags| !flags.is_empty()) else {
            break;
        };
        if argument == "--" {
            break;
        }
        let mut flags = flags.chars().peekable();
        while let Some(flag) = flags.next() {
            if flag == 'k' {
                return true;
            }
            if spec.takes_value(flag) {
                if flags.peek().is_none() {
                    let _ = args.next();
                }
                break;
            }
        }
    }
    false
}

/// Whether already-canonical `new-pane` argv asks for its finished input.
///
/// `-I` is read as an option and only as one: a value that happens to look like
/// a flag, and everything after `--`, belong to the command, not to this
/// decision. The same walk the split classifier does, with this command's own
/// short-option inventory.
#[must_use]
pub fn new_pane_requires_input(command: &crate::command_parser::ParsedCommand) -> bool {
    if command.name() != "new-pane" {
        return false;
    }
    let spec = crate::command_inventory::command_short_option_spec("new-pane")
        .expect("new-pane has a short-option inventory");
    let mut args = command.arguments().iter();
    while let Some(argument) = args.next().and_then(|argument| argument.as_string()) {
        if argument == "--" {
            break;
        }
        let Some(flags) = argument.strip_prefix('-').filter(|flags| !flags.is_empty()) else {
            break;
        };
        let mut flags = flags.chars().peekable();
        while let Some(flag) = flags.next() {
            if flag == 'I' {
                return true;
            }
            if spec.takes_value(flag) {
                if flags.peek().is_none() {
                    let _ = args.next();
                }
                break;
            }
        }
    }
    false
}

/// Display forms whose full CLI owner sends a source queue, including its
/// cancellation policy and parsed hook arguments. JSON has a separate owner.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayMessageEntry {
    /// Enumerate all format variables (`-a`).
    pub all_formats: bool,
    /// Forward standard input (`-I`).
    pub stdin: bool,
    /// Suppress format expansion (`-l`).
    pub literal: bool,
    /// Explain format expansion (`-v`).
    pub verbose: bool,
    /// An explicit pane target was supplied (`-t`).
    pub has_target: bool,
}

impl DisplayMessageEntry {
    /// Whether the non-JSON CLI enters through its source-queue owner.
    #[must_use]
    pub const fn uses_source_queue(self) -> bool {
        self.all_formats || self.stdin || self.literal || self.verbose || self.has_target
    }
}

/// Canonical argv sent by the full CLI's all-session window-list owner.
#[must_use]
pub fn list_windows_all_queue_arguments(
    target: Option<&str>,
    format: Option<&str>,
    filter: Option<&str>,
    sort_order: Option<&str>,
    reversed: bool,
) -> Vec<String> {
    let mut arguments = vec!["list-windows".to_owned(), "-a".to_owned()];
    for (flag, value) in [
        ("-t", target),
        ("-F", format),
        ("-f", filter),
        ("-O", sort_order),
    ] {
        if let Some(value) = value {
            arguments.extend([flag.to_owned(), value.to_owned()]);
        }
    }
    if reversed {
        arguments.push("-r".to_owned());
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fstdin1_classification_only_observes_the_input_flag_not_values_or_the_tail() {
        for (argv, expected) in [
            (vec!["new-pane", "-I"], true),
            (vec!["new-pane", "-dI"], true),
            (vec!["new-pane", "-F", "-I"], false),
            (vec!["new-pane", "-x", "-I", "-d"], false),
            (vec!["new-pane", "--", "-I"], false),
            (vec!["new-pane", "printf", "-I"], false),
            (vec!["new-pane", "-d"], false),
            (vec!["split-window", "-I"], false),
        ] {
            let parsed = crate::command_parser::CommandParser::new()
                .with_exact_commands(crate::command_inventory::RMUX_EXTENSION_COMMANDS)
                .parse_arguments(&argv)
                .unwrap();
            assert_eq!(
                new_pane_requires_input(&parsed.commands()[0]),
                expected,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn sc2_classification_only_observes_split_flags_not_values_or_command_tail() {
        for (argv, expected) in [
            (vec!["split-window", "-Ik"], true),
            (vec!["splitw", "-dkP"], true),
            (vec!["split-window", "-F", "-k", "-I"], false),
            (vec!["split-window", "-Fk", "-I"], false),
            (vec!["split-window", "--", "-k"], false),
            (vec!["split-window", "printf", "-k"], false),
            (vec!["new-window", "-k"], false),
        ] {
            let parsed = crate::command_parser::CommandParser::new()
                .parse_arguments(&argv)
                .unwrap();
            assert_eq!(
                split_command_requires_key_admission(&parsed.commands()[0]),
                expected,
                "{argv:?}"
            );
        }
    }
}
