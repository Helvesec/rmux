use super::source_files::SourceInput;

pub(super) fn tmux_compat_input(input: &SourceInput) -> SourceInput {
    SourceInput {
        current_file: input.current_file.clone(),
        contents: quiet_optional_tmux_conf_local_source(&input.contents),
    }
}

fn quiet_optional_tmux_conf_local_source(contents: &str) -> String {
    let mut rewritten = String::with_capacity(contents.len());
    let mut cursor = 0;
    while let Some(command) = find_optional_tmux_conf_local_source_command(contents, cursor) {
        rewritten.push_str(&contents[cursor..command.end]);
        rewritten.push_str(" -q");
        cursor = command.end;
    }
    rewritten.push_str(&contents[cursor..]);
    rewritten
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceCommandMatch {
    end: usize,
}

fn find_optional_tmux_conf_local_source_command(
    contents: &str,
    offset: usize,
) -> Option<SourceCommandMatch> {
    for (relative_index, _) in contents[offset..].char_indices() {
        let start = offset + relative_index;
        let Some(end) = source_command_end_at(contents, start) else {
            continue;
        };
        if !source_command_has_gpakosz_run_context(contents, start) {
            continue;
        }
        if source_command_loads_tmux_conf_local(&contents[end..]) {
            return Some(SourceCommandMatch { end });
        }
    }
    None
}

fn source_command_end_at(contents: &str, start: usize) -> Option<usize> {
    if !is_source_command_boundary(contents[..start].chars().next_back()) {
        return None;
    }

    let end = if contents[start..].starts_with("source-file") {
        start + "source-file".len()
    } else if contents[start..].starts_with("source") {
        start + "source".len()
    } else {
        return None;
    };

    if is_source_command_boundary(contents[end..].chars().next()) {
        Some(end)
    } else {
        None
    }
}

fn is_source_command_boundary(character: Option<char>) -> bool {
    character.is_none_or(|character| {
        character.is_whitespace()
            || matches!(character, ';' | '"' | '\'' | '{' | '(' | ')' | '[' | ']')
    })
}

fn source_command_has_gpakosz_run_context(contents: &str, source_start: usize) -> bool {
    let line_start = contents[..source_start]
        .rfind(['\n', '\r'])
        .map_or(0, |index| index + 1);
    let prefix = &contents[line_start..source_start];
    let trimmed = prefix.trim_start();
    let Some(after_run) =
        strip_command_word(trimmed, "run").or_else(|| strip_command_word(trimmed, "run-shell"))
    else {
        return false;
    };
    let Some(after_program) = strip_expected_tmux_program_invocation(after_run) else {
        return false;
    };
    tmux_program_prefix_args_are_safe(after_program)
}

fn strip_command_word<'a>(input: &'a str, command: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(command)?;
    rest.chars()
        .next()
        .is_none_or(|character| character.is_whitespace())
        .then_some(rest)
}

fn strip_expected_tmux_program_invocation(after_run: &str) -> Option<&str> {
    let mut rest = after_run.trim_start();
    while let Some(after_flag) = strip_run_shell_flag(rest) {
        rest = after_flag.trim_start();
    }
    if matches!(rest.chars().next(), Some('\'' | '"')) {
        rest = &rest[1..];
        rest = rest.trim_start();
    }
    for token in [
        "\"$TMUX_PROGRAM\"",
        "\"${TMUX_PROGRAM}\"",
        "$TMUX_PROGRAM",
        "${TMUX_PROGRAM}",
    ] {
        if let Some(after_program) = rest.strip_prefix(token) {
            return Some(after_program);
        }
    }
    None
}

fn strip_run_shell_flag(input: &str) -> Option<&str> {
    let rest = input.strip_prefix('-')?;
    let flag_end = rest
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .map_or(rest.len(), |index| index);
    let flag = &rest[..flag_end];
    if flag.is_empty()
        || flag
            .chars()
            .any(|character| !character.is_ascii_alphabetic())
    {
        return None;
    }
    let mut after_flag = &rest[flag_end..];
    if flag.chars().any(|character| matches!(character, 'd' | 't')) {
        after_flag = skip_one_shell_word(after_flag.trim_start())?;
    }
    Some(after_flag)
}

fn skip_one_shell_word(input: &str) -> Option<&str> {
    let quote = input
        .chars()
        .next()
        .filter(|character| matches!(character, '\'' | '"'));
    let mut escaped = false;
    for (index, character) in input.char_indices().skip(usize::from(quote.is_some())) {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if Some(character) == quote {
            return Some(&input[index + character.len_utf8()..]);
        }
        if quote.is_none() && character.is_whitespace() {
            return Some(&input[index..]);
        }
    }
    quote.is_none().then_some("")
}

fn tmux_program_prefix_args_are_safe(after_program: &str) -> bool {
    if after_program
        .chars()
        .any(|character| matches!(character, ';' | '|' | '&' | '<' | '>' | '`'))
        || after_program.contains("$(")
    {
        return false;
    }
    let mut expect_value = false;
    for token in after_program.split_whitespace() {
        if expect_value {
            expect_value = false;
            continue;
        }
        let Some(flags) = token.strip_prefix('-') else {
            return false;
        };
        if flags.is_empty()
            || flags
                .chars()
                .any(|character| !character.is_ascii_alphabetic())
        {
            return false;
        }
        expect_value = flags
            .chars()
            .any(|character| matches!(character, 'S' | 'L' | 'f'));
    }
    !expect_value
}

fn source_command_loads_tmux_conf_local(rest: &str) -> bool {
    let mut cursor = 0;
    let mut quiet = false;
    while let Some(word) = next_source_command_word(rest, cursor) {
        cursor = word.end;
        let normalized = normalize_source_command_word(word.text);
        if normalized.is_empty() {
            continue;
        }
        if normalized.starts_with('-') {
            if normalized.chars().skip(1).any(|character| character == 'q') {
                quiet = true;
            }
            continue;
        }
        return !quiet && normalized == "$TMUX_CONF_LOCAL";
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceCommandWord<'a> {
    text: &'a str,
    end: usize,
}

fn next_source_command_word(rest: &str, offset: usize) -> Option<SourceCommandWord<'_>> {
    let mut start = offset;
    while let Some(character) = rest[start..].chars().next() {
        if matches!(character, ' ' | '\t') {
            start += character.len_utf8();
            continue;
        }
        break;
    }

    let first = rest[start..].chars().next()?;
    if matches!(first, '\n' | '\r' | ';') {
        return None;
    }

    let mut end = start;
    for (relative_index, character) in rest[start..].char_indices() {
        if character.is_whitespace() || character == ';' {
            break;
        }
        end = start + relative_index + character.len_utf8();
    }
    Some(SourceCommandWord {
        text: &rest[start..end],
        end,
    })
}

fn normalize_source_command_word(word: &str) -> String {
    let normalized = word
        .chars()
        .filter(|character| !matches!(character, '"' | '\''))
        .collect::<String>();
    if normalized == "${TMUX_CONF_LOCAL}" {
        "$TMUX_CONF_LOCAL".to_owned()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::tmux_compat_input;
    use crate::handler::scripting_support::source_files::SourceInput;

    #[test]
    fn tmux_compat_input_preserves_runtime_commands() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents: "set -g status off\nrun-shell 'true'\nset -g @plugin 'tmux-plugins/tpm'\n"
                .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(preserved.current_file, input.current_file);
        assert_eq!(preserved.contents, input.contents);
    }

    #[test]
    fn tmux_compat_input_quiets_optional_tmux_conf_local_source() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents:
                "run '\"$TMUX_PROGRAM\" -S #{socket_path} source \"$TMUX_CONF_LOCAL\"'\n\
                       run '\"$TMUX_PROGRAM\" -S #{socket_path} source-file \"$TMUX_CONF_LOCAL\"'\n\
                       run '\"$TMUX_PROGRAM\" -S #{socket_path} source-file\t\"$TMUX_CONF_LOCAL\"'\n"
                    .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(
            preserved.contents,
            "run '\"$TMUX_PROGRAM\" -S #{socket_path} source -q \"$TMUX_CONF_LOCAL\"'\n\
             run '\"$TMUX_PROGRAM\" -S #{socket_path} source-file -q \"$TMUX_CONF_LOCAL\"'\n\
             run '\"$TMUX_PROGRAM\" -S #{socket_path} source-file -q\t\"$TMUX_CONF_LOCAL\"'\n"
        );
    }

    #[test]
    fn tmux_compat_input_does_not_requiet_existing_quiet_source() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents: "run '\"$TMUX_PROGRAM\" source -q \"$TMUX_CONF_LOCAL\"'\n\
                 run '\"$TMUX_PROGRAM\" source-file -q \"$TMUX_CONF_LOCAL\"'\n"
                .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(preserved.contents, input.contents);
    }

    #[test]
    fn tmux_compat_input_does_not_quiet_unrelated_source_targets() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents: "run '\"$TMUX_PROGRAM\" source \"$OTHER_CONF\"'\n\
                 run '\"$TMUX_PROGRAM\" source-file \"$OTHER_CONF\" \"$TMUX_CONF_LOCAL\"'\n\
                 run '\"$TMUX_PROGRAM\" resource \"$TMUX_CONF_LOCAL\"'\n"
                .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(preserved.contents, input.contents);
    }

    #[test]
    fn tmux_compat_input_quiets_braced_tmux_conf_local_source() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents: "run '\"$TMUX_PROGRAM\" -S #{socket_path} source \"${TMUX_CONF_LOCAL}\"'\n"
                .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(
            preserved.contents,
            "run '\"$TMUX_PROGRAM\" -S #{socket_path} source -q \"${TMUX_CONF_LOCAL}\"'\n"
        );
    }

    #[test]
    fn tmux_compat_input_does_not_requiet_combined_quiet_flags() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents: "run '\"$TMUX_PROGRAM\" source -Fq \"$TMUX_CONF_LOCAL\"'\n\
                 run '\"$TMUX_PROGRAM\" source-file -qF \"$TMUX_CONF_LOCAL\"'\n"
                .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(preserved.contents, input.contents);
    }

    #[test]
    fn tmux_compat_input_does_not_rewrite_literal_option_values() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents: "set -g @literal 'source \"$TMUX_CONF_LOCAL\"'\n\
                 set -g @literal_tmux_program '\"$TMUX_PROGRAM\" source \"$TMUX_CONF_LOCAL\"'\n"
                .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(preserved.contents, input.contents);
    }

    #[test]
    fn tmux_compat_input_does_not_rewrite_run_shell_payload_literals() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents:
                "run 'TMUX_PROGRAM=rmux; printf \"%s\\n\" source \"$TMUX_CONF_LOCAL\" > \"$out\"'\n\
                 run '\"$TMUX_PROGRAM\"; printf \"%s\\n\" source \"$TMUX_CONF_LOCAL\" > \"$out\"'\n"
                    .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(preserved.contents, input.contents);
    }
}
