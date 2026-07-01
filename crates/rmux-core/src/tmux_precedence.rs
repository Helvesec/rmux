//! tmux-compatible option precedence normalization for command arguments.
//!
//! tmux accepts many repeated flags that clap-style parsers would normally
//! reject as mutually exclusive. This module rewrites only the option prefix of
//! supported commands so every RMUX command path observes tmux's command-specific
//! precedence behaviour before command-specific parsing.

use std::collections::BTreeSet;

/// Returns `arguments` rewritten with tmux-compatible precedence rules for
/// `command_name`.
#[must_use]
pub fn normalize_tmux_precedence(command_name: &str, arguments: Vec<String>) -> Vec<String> {
    match command_name {
        "split-window" => normalize_split_window(arguments),
        "resize-pane" => normalize_resize_pane(arguments),
        "join-pane" | "move-pane" => collapse_short_flag_group_by_priority_in_option_prefix(
            arguments,
            &['h', 'v'],
            &BTreeSet::from(['l', 'p', 's', 't']),
        ),
        "select-pane" => collapse_short_flag_group_by_priority_in_option_prefix(
            arguments,
            &['L', 'R', 'U', 'D'],
            &BTreeSet::from(['P', 'T', 't']),
        ),
        "select-window" => collapse_short_flag_group_by_priority_in_option_prefix(
            arguments,
            &['n', 'p', 'l'],
            &BTreeSet::from(['t']),
        ),
        "new-window" => collapse_short_flag_group_by_priority_in_option_prefix(
            arguments,
            &['b', 'a'],
            &BTreeSet::from(['c', 'e', 'F', 'n', 't']),
        ),
        "move-window" | "link-window" | "break-pane" => {
            collapse_short_flag_group_by_priority_in_option_prefix(
                arguments,
                &['b', 'a'],
                &BTreeSet::from(['F', 'n', 's', 't']),
            )
        }
        "swap-pane" => collapse_short_flag_group_by_priority_in_option_prefix(
            arguments,
            &['D', 'U'],
            &BTreeSet::from(['s', 't']),
        ),
        _ => arguments,
    }
}

fn normalize_split_window(arguments: Vec<String>) -> Vec<String> {
    collapse_short_flag_group_by_priority_in_option_prefix(
        arguments,
        &['h', 'v'],
        &BTreeSet::from(['c', 'e', 'F', 'l', 'p', 't']),
    )
}

fn normalize_resize_pane(arguments: Vec<String>) -> Vec<String> {
    collapse_short_flag_group_by_priority_in_option_prefix(
        arguments,
        &['L', 'R', 'U', 'D'],
        &BTreeSet::from(['t', 'x', 'y']),
    )
}

fn collapse_short_flag_group_by_priority_in_option_prefix(
    arguments: Vec<String>,
    priority_flags: &[char],
    value_flags: &BTreeSet<char>,
) -> Vec<String> {
    let present = short_flag_group_flags_present(&arguments, priority_flags, value_flags);
    let Some(selected) = priority_flags
        .iter()
        .copied()
        .find(|flag| present.contains(flag))
    else {
        return arguments;
    };
    let group_flags = priority_flags.iter().copied().collect::<BTreeSet<_>>();

    let mut selected_emitted = false;
    let mut output = Vec::with_capacity(arguments.len());
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            output.extend(arguments[index..].iter().cloned());
            break;
        }
        if is_trailing_position_start(argument) {
            output.extend(arguments[index..].iter().cloned());
            break;
        }
        if let Some(stripped) = short_flag_cluster(argument) {
            let mut rewritten = String::from("-");
            let mut consumes_value = false;
            let mut emitted_value_flag = false;
            let mut chars = stripped.char_indices().peekable();
            while let Some((_, flag)) = chars.next() {
                let value_start = chars.peek().map_or(stripped.len(), |(index, _)| *index);
                let keep = if group_flags.contains(&flag) {
                    if flag == selected && !selected_emitted {
                        selected_emitted = true;
                        true
                    } else {
                        false
                    }
                } else {
                    true
                };

                if value_flags.contains(&flag) {
                    if keep {
                        if rewritten.len() > 1 {
                            output.push(rewritten);
                            rewritten = String::from("-");
                        }
                        output.push(format!("-{flag}"));
                    }
                    emitted_value_flag = keep;
                    let attached_value = &stripped[value_start..];
                    if attached_value.is_empty() {
                        consumes_value = true;
                    } else if keep {
                        output.push(attached_value.to_owned());
                    }
                    break;
                }

                if keep {
                    rewritten.push(flag);
                }
            }
            if !emitted_value_flag && rewritten.len() > 1 {
                output.push(rewritten);
            }
            if consumes_value && index + 1 < arguments.len() {
                index += 1;
                if emitted_value_flag {
                    output.push(arguments[index].clone());
                }
            }
        } else {
            output.push(argument.clone());
        }
        index += 1;
    }
    output
}

fn short_flag_group_flags_present(
    arguments: &[String],
    priority_flags: &[char],
    value_flags: &BTreeSet<char>,
) -> BTreeSet<char> {
    let group_flags = priority_flags.iter().copied().collect::<BTreeSet<_>>();
    let mut present = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" || is_trailing_position_start(argument) {
            break;
        }
        if let Some(stripped) = short_flag_cluster(argument) {
            let mut chars = stripped.char_indices().peekable();
            while let Some((_, flag)) = chars.next() {
                let value_start = chars.peek().map_or(stripped.len(), |(index, _)| *index);
                if group_flags.contains(&flag) {
                    present.insert(flag);
                }
                if value_flags.contains(&flag) {
                    if stripped[value_start..].is_empty() {
                        index += 1;
                    }
                    break;
                }
            }
        }
        index += 1;
    }
    present
}

fn short_flag_cluster(argument: &str) -> Option<&str> {
    let stripped = argument.strip_prefix('-')?;
    (!stripped.is_empty() && !stripped.starts_with('-')).then_some(stripped)
}

fn is_trailing_position_start(argument: &str) -> bool {
    !argument.starts_with('-') || argument == "-"
}

#[cfg(test)]
mod tests {
    use super::normalize_tmux_precedence;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn resize_pane_preserves_zoom_with_other_adjustments_like_tmux() {
        assert_eq!(
            normalize_tmux_precedence("resize-pane", args(&["-Z", "-R", "-t", "a:0.0"])),
            args(&["-Z", "-R", "-t", "a:0.0"])
        );
        assert_eq!(
            normalize_tmux_precedence("resize-pane", args(&["-Z", "-x", "34", "-t", "a:0.0"])),
            args(&["-Z", "-x", "34", "-t", "a:0.0"])
        );
    }

    #[test]
    fn resize_pane_relative_directions_follow_tmux_priority() {
        for input in [
            args(&["-R", "-L", "-t", "a:0.0"]),
            args(&["-L", "-R", "-t", "a:0.0"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("resize-pane", input),
                args(&["-L", "-t", "a:0.0"])
            );
        }
        for input in [
            args(&["-D", "-U", "-t", "a:0.0"]),
            args(&["-U", "-D", "-t", "a:0.0"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("resize-pane", input),
                args(&["-U", "-t", "a:0.0"])
            );
        }
        assert_eq!(
            normalize_tmux_precedence("resize-pane", args(&["-R", "-L", "-t", "a:0.0", "3"])),
            args(&["-L", "-t", "a:0.0", "3"])
        );
    }

    #[test]
    fn resize_pane_preserves_absolute_and_relative_composition_like_tmux() {
        assert_eq!(
            normalize_tmux_precedence("resize-pane", args(&["-x", "30", "-R", "-t", "a:0.0"])),
            args(&["-x", "30", "-R", "-t", "a:0.0"])
        );
        assert_eq!(
            normalize_tmux_precedence("resize-pane", args(&["-R", "-x", "30", "-t", "a:0.0"])),
            args(&["-R", "-x", "30", "-t", "a:0.0"])
        );
    }

    #[test]
    fn split_window_preserves_legacy_percentage_modifier_without_overriding_size() {
        assert_eq!(
            normalize_tmux_precedence("split-window", args(&["-l", "5", "-p", "50", "-t", "a"])),
            args(&["-l", "5", "-p", "50", "-t", "a"])
        );
        assert_eq!(
            normalize_tmux_precedence("split-window", args(&["-p", "50", "-l", "5", "-t", "a"])),
            args(&["-p", "50", "-l", "5", "-t", "a"])
        );
    }

    #[test]
    fn split_window_and_navigation_groups_follow_tmux_priority() {
        for input in [
            args(&["-h", "-v", "-t", "a"]),
            args(&["-v", "-h", "-t", "a"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("split-window", input),
                args(&["-h", "-t", "a"])
            );
        }
        for command in ["join-pane", "move-pane"] {
            for input in [
                args(&["-h", "-v", "-s", "a:0.1", "-t", "a:0.0"]),
                args(&["-v", "-h", "-s", "a:0.1", "-t", "a:0.0"]),
                args(&["-hv", "-s", "a:0.1", "-t", "a:0.0"]),
                args(&["-vh", "-s", "a:0.1", "-t", "a:0.0"]),
            ] {
                assert_eq!(
                    normalize_tmux_precedence(command, input),
                    args(&["-h", "-s", "a:0.1", "-t", "a:0.0"])
                );
            }
        }
        for input in [
            args(&["-L", "-R", "-t", "a:0.0"]),
            args(&["-R", "-L", "-t", "a:0.0"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("select-pane", input),
                args(&["-L", "-t", "a:0.0"])
            );
        }
        assert_eq!(
            normalize_tmux_precedence("select-pane", args(&["-D", "-U", "-t", "a:0.0"])),
            args(&["-U", "-t", "a:0.0"])
        );
        for input in [
            args(&["-n", "-p", "-t", "a"]),
            args(&["-p", "-n", "-t", "a"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("select-window", input),
                args(&["-n", "-t", "a"])
            );
        }
        assert_eq!(
            normalize_tmux_precedence("select-window", args(&["-l", "-p", "-t", "a"])),
            args(&["-p", "-t", "a"])
        );
    }

    #[test]
    fn placement_and_swap_groups_follow_tmux_priority() {
        for input in [
            args(&["-a", "-b", "-t", "a", "sh"]),
            args(&["-b", "-a", "-t", "a", "sh"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("new-window", input),
                args(&["-b", "-t", "a", "sh"])
            );
        }
        for input in [
            args(&["-a", "-b", "-s", "a:1", "-t", "a:0"]),
            args(&["-b", "-a", "-s", "a:1", "-t", "a:0"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("move-window", input),
                args(&["-b", "-s", "a:1", "-t", "a:0"])
            );
        }
        for input in [
            args(&["-D", "-U", "-s", "a:0.0", "-t", "a:0.1"]),
            args(&["-U", "-D", "-s", "a:0.0", "-t", "a:0.1"]),
        ] {
            assert_eq!(
                normalize_tmux_precedence("swap-pane", input),
                args(&["-D", "-s", "a:0.0", "-t", "a:0.1"])
            );
        }
    }
}
