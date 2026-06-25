use std::collections::BTreeSet;

pub(super) fn normalize(command_name: &str, arguments: Vec<String>) -> Vec<String> {
    match command_name {
        "split-window" => normalize_split_window(arguments),
        "resize-pane" => normalize_resize_pane(arguments),
        _ => arguments,
    }
}

fn normalize_split_window(arguments: Vec<String>) -> Vec<String> {
    let arguments = collapse_short_flag_group_in_option_prefix(
        arguments,
        &BTreeSet::from(['h', 'v']),
        &BTreeSet::from(['c', 'e', 'F', 'l', 'p', 't']),
    );
    collapse_value_flag_group_in_option_prefix(
        arguments,
        &BTreeSet::from(['l', 'p']),
        &BTreeSet::from(['c', 'e', 'F', 'l', 'p', 't']),
    )
}

fn normalize_resize_pane(arguments: Vec<String>) -> Vec<String> {
    let occurrences = resize_adjustment_occurrences(&arguments);
    if occurrences.len() <= 1 {
        return arguments;
    }

    if occurrences
        .iter()
        .take(occurrences.len().saturating_sub(1))
        .any(|occurrence| occurrence.has_explicit_relative_value)
    {
        return arguments;
    }

    drop_occurrences_except_last(arguments, occurrences)
}

#[derive(Clone)]
struct OptionOccurrence {
    indexes: Vec<usize>,
    has_explicit_relative_value: bool,
    kind: OptionOccurrenceKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OptionOccurrenceKind {
    AbsoluteResize,
    Other,
}

fn resize_adjustment_occurrences(arguments: &[String]) -> Vec<OptionOccurrence> {
    let mut occurrences = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--" => break,
            "-D" | "-U" | "-L" | "-R" => {
                let explicit_value = arguments
                    .get(index + 1)
                    .is_some_and(|value| !value.starts_with('-'));
                let indexes = if explicit_value {
                    vec![index, index + 1]
                } else {
                    vec![index]
                };
                occurrences.push(OptionOccurrence {
                    indexes,
                    has_explicit_relative_value: explicit_value,
                    kind: OptionOccurrenceKind::Other,
                });
                index += if explicit_value { 2 } else { 1 };
            }
            "-x" | "-y" => {
                let mut indexes = vec![index];
                if index + 1 < arguments.len() {
                    indexes.push(index + 1);
                    index += 2;
                } else {
                    index += 1;
                }
                if let Some(last) = occurrences
                    .last_mut()
                    .filter(|occurrence| occurrence.kind == OptionOccurrenceKind::AbsoluteResize)
                {
                    last.indexes.extend(indexes);
                } else {
                    occurrences.push(OptionOccurrence {
                        indexes,
                        has_explicit_relative_value: false,
                        kind: OptionOccurrenceKind::AbsoluteResize,
                    });
                }
            }
            "-Z" => {
                occurrences.push(OptionOccurrence {
                    indexes: vec![index],
                    has_explicit_relative_value: false,
                    kind: OptionOccurrenceKind::Other,
                });
                index += 1;
            }
            _ => index += 1,
        }
    }
    occurrences
}

fn collapse_short_flag_group_in_option_prefix(
    arguments: Vec<String>,
    group_flags: &BTreeSet<char>,
    value_flags: &BTreeSet<char>,
) -> Vec<String> {
    let Some(last) = last_short_flag_group_occurrence(&arguments, group_flags, value_flags) else {
        return arguments;
    };

    let mut occurrence = 0usize;
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
                    let keep = occurrence == last;
                    occurrence += 1;
                    keep
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
                output.push(arguments[index].clone());
            }
        } else {
            output.push(argument.clone());
        }
        index += 1;
    }
    output
}

fn last_short_flag_group_occurrence(
    arguments: &[String],
    group_flags: &BTreeSet<char>,
    value_flags: &BTreeSet<char>,
) -> Option<usize> {
    let mut last = None;
    let mut occurrence = 0usize;
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
                    last = Some(occurrence);
                    occurrence += 1;
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
    last
}

fn collapse_value_flag_group_in_option_prefix(
    arguments: Vec<String>,
    group_flags: &BTreeSet<char>,
    value_flags: &BTreeSet<char>,
) -> Vec<String> {
    let occurrences = value_flag_group_occurrences(&arguments, group_flags, value_flags);
    if occurrences.len() <= 1 {
        return arguments;
    }
    drop_occurrences_except_last(arguments, occurrences)
}

fn value_flag_group_occurrences(
    arguments: &[String],
    group_flags: &BTreeSet<char>,
    value_flags: &BTreeSet<char>,
) -> Vec<OptionOccurrence> {
    let mut occurrences = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" || is_trailing_position_start(argument) {
            break;
        }
        if let Some(flag) = exact_short_flag(argument, value_flags) {
            let has_value = index + 1 < arguments.len();
            if group_flags.contains(&flag) {
                let indexes = if has_value {
                    vec![index, index + 1]
                } else {
                    vec![index]
                };
                occurrences.push(OptionOccurrence {
                    indexes,
                    has_explicit_relative_value: false,
                    kind: OptionOccurrenceKind::Other,
                });
            }
            index += if has_value { 2 } else { 1 };
        } else {
            index += 1;
        }
    }
    occurrences
}

fn drop_occurrences_except_last(
    arguments: Vec<String>,
    occurrences: Vec<OptionOccurrence>,
) -> Vec<String> {
    let mut drop_indexes = BTreeSet::new();
    for occurrence in occurrences.iter().take(occurrences.len().saturating_sub(1)) {
        drop_indexes.extend(occurrence.indexes.iter().copied());
    }

    arguments
        .into_iter()
        .enumerate()
        .filter_map(|(index, argument)| (!drop_indexes.contains(&index)).then_some(argument))
        .collect()
}

fn short_flag_cluster(argument: &str) -> Option<&str> {
    let stripped = argument.strip_prefix('-')?;
    (!stripped.is_empty() && !stripped.starts_with('-')).then_some(stripped)
}

fn is_trailing_position_start(argument: &str) -> bool {
    !argument.starts_with('-') || argument == "-"
}

fn exact_short_flag(argument: &str, flags: &BTreeSet<char>) -> Option<char> {
    let mut chars = argument.chars();
    if chars.next()? != '-' || chars.as_str().starts_with('-') {
        return None;
    }
    let flag = chars.next()?;
    (chars.next().is_none() && flags.contains(&flag)).then_some(flag)
}
