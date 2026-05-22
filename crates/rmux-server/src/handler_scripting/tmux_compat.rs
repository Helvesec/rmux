use super::command_args::command_arguments_as_strings;
use super::config_parse::parse_set_option;
use super::key_parse::parse_unbind_key;
use super::source_files::SourceInput;
use super::tokens::CommandTokens;
use rmux_core::command_parser::{CommandParser, ParsedCommand, ParsedCommands};
use rmux_core::{key_string_lookup_string, KeyBindingStore, OptionStore, KEYC_NONE, KEYC_UNKNOWN};
use rmux_proto::Request;

#[path = "tmux_compat/options.rs"]
mod options;

use options::allowed_static_option_name;

pub(super) fn filter_tmux_compat_input(input: &SourceInput) -> SourceInput {
    let mut contents = String::new();
    let mut conditional_depth = 0usize;
    let mut command_block_depth = 0usize;
    for logical_line in tmux_logical_lines(&input.contents) {
        if command_block_depth != 0 {
            update_command_block_depth(&logical_line, &mut command_block_depth);
            continue;
        }
        if update_conditional_depth(&logical_line, &mut conditional_depth) {
            continue;
        }
        if conditional_depth != 0 {
            continue;
        }
        if starts_unsupported_command_block(&logical_line, &mut command_block_depth) {
            continue;
        }
        if let Some(line) = compatible_line(&logical_line) {
            contents.push_str(&line);
            contents.push('\n');
        }
    }

    SourceInput {
        current_file: input.current_file.clone(),
        contents,
    }
}

fn tmux_logical_lines(contents: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for line in contents.lines() {
        let trimmed_end = line.trim_end();
        if has_line_continuation(trimmed_end) {
            current.push_str(trimmed_end.trim_end_matches('\\'));
            continue;
        }

        current.push_str(line);
        lines.push(std::mem::take(&mut current));
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

fn has_line_continuation(line: &str) -> bool {
    let slash_count = line.chars().rev().take_while(|ch| *ch == '\\').count();
    slash_count % 2 == 1
}

fn update_conditional_depth(line: &str, depth: &mut usize) -> bool {
    match conditional_directive(line) {
        Some("%if") => *depth = depth.saturating_add(1),
        Some("%endif") => *depth = depth.saturating_sub(1),
        Some("%elif" | "%else") => {}
        _ => return false,
    }
    true
}

fn conditional_directive(line: &str) -> Option<&str> {
    match line.split_whitespace().next()? {
        directive @ ("%if" | "%elif" | "%else" | "%endif") => Some(directive),
        _ => None,
    }
}

fn starts_unsupported_command_block(line: &str, depth: &mut usize) -> bool {
    let net_braces = net_brace_depth(line);
    if net_braces == 0 {
        return false;
    }

    let parsed = CommandParser::new().parse(line);
    let supported = parsed
        .as_ref()
        .ok()
        .filter(|commands| commands.assignments().is_empty())
        .and_then(compatible_commands)
        .is_some();
    if supported {
        return false;
    }

    *depth = net_braces;
    true
}

fn update_command_block_depth(line: &str, depth: &mut usize) {
    *depth = apply_brace_depth(line, *depth);
}

fn net_brace_depth(line: &str) -> usize {
    apply_brace_depth(line, 0)
}

fn apply_brace_depth(line: &str, initial_depth: usize) -> usize {
    let mut depth = initial_depth;
    let mut quote = None;
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }

        if let Some(quote_ch) = quote {
            if ch == '\\' {
                escaped = true;
            } else if ch == quote_ch {
                quote = None;
            }
            continue;
        }

        match ch {
            '#' => break,
            '\\' => escaped = true,
            '\'' | '"' => quote = Some(ch),
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    depth
}

fn compatible_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('%') {
        return None;
    }

    let parsed = CommandParser::new().parse(line).ok()?;
    if !parsed.assignments().is_empty() {
        return None;
    }

    compatible_commands(&parsed).map(|commands| commands.join(" ; "))
}

fn compatible_commands(parsed: &ParsedCommands) -> Option<Vec<String>> {
    (!parsed.is_empty())
        .then(|| {
            parsed
                .commands()
                .iter()
                .map(compatible_command)
                .collect::<Option<Vec<_>>>()
        })
        .flatten()
}

fn compatible_command(command: &ParsedCommand) -> Option<String> {
    match command.name() {
        "set-option" => validate_set_option(command, false),
        "set-window-option" => validate_set_option(command, true),
        "unbind-key" => validate_unbind_key(command),
        _ => false,
    }
    .then(|| compatible_command_string(command))
    .flatten()
}

fn validate_set_option(command: &ParsedCommand, force_window: bool) -> bool {
    let Ok(arguments) = command_arguments_as_strings(command.name(), command.arguments()) else {
        return false;
    };
    let arguments = expand_leading_compact_flags(arguments, "gswpaouU");
    let Ok(Request::SetOptionByName(request)) =
        parse_set_option(CommandTokens::new(arguments), force_window)
    else {
        return false;
    };

    if request.name.starts_with('@') {
        return false;
    }
    if !allowed_static_option_name(&request.name) {
        return false;
    }
    if request.value.as_deref().is_some_and(contains_format_job) {
        return false;
    }

    OptionStore::new()
        .set_by_name(
            request.scope,
            &request.name,
            request.value,
            request.mode,
            request.only_if_unset,
            request.unset,
            request.unset_pane_overrides,
        )
        .is_ok()
}

fn validate_unbind_key(command: &ParsedCommand) -> bool {
    let Ok(arguments) = command_arguments_as_strings(command.name(), command.arguments()) else {
        return false;
    };
    let arguments = expand_leading_compact_flags(arguments, "anq");
    let Ok(Request::UnbindKey(request)) = parse_unbind_key(CommandTokens::new(arguments)) else {
        return false;
    };

    if request.all == request.key.is_some() {
        return false;
    }
    if request.all {
        return false;
    }
    if request.key.as_deref().is_some_and(|key| !known_key(key)) {
        return false;
    }
    if !request.quiet
        && KeyBindingStore::default()
            .table(&request.table_name)
            .is_none()
    {
        return false;
    }

    true
}

fn known_key(key: &str) -> bool {
    key_string_lookup_string(key).is_some_and(|key| key != KEYC_NONE && key != KEYC_UNKNOWN)
}

fn contains_format_job(value: &str) -> bool {
    value.contains("#(")
}

fn compatible_command_string(command: &ParsedCommand) -> Option<String> {
    let arguments = command_arguments_as_strings(command.name(), command.arguments()).ok()?;
    let arguments = match command.name() {
        "set-option" | "set-window-option" => expand_leading_compact_flags(arguments, "gswpaouU"),
        "unbind-key" => expand_leading_compact_flags(arguments, "anq"),
        _ => arguments,
    };
    Some(
        std::iter::once(command.name().to_owned())
            .chain(arguments.into_iter().map(escape_argument))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn expand_leading_compact_flags(arguments: Vec<String>, no_arg_flags: &str) -> Vec<String> {
    let mut expanded = Vec::with_capacity(arguments.len());
    let mut iter = arguments.into_iter().peekable();

    while let Some(argument) = iter.next() {
        if argument == "--" {
            expanded.push(argument);
            expanded.extend(iter);
            return expanded;
        }

        if !argument.starts_with('-') || argument == "-" {
            expanded.push(argument);
            expanded.extend(iter);
            return expanded;
        }

        if argument.starts_with("--") || argument.chars().count() <= 2 {
            expanded.push(argument);
            continue;
        }

        let compact_flags = &argument[1..];
        if compact_flags.chars().all(|ch| no_arg_flags.contains(ch)) {
            expanded.extend(compact_flags.chars().map(|ch| format!("-{ch}")));
        } else {
            expanded.push(argument);
        }
    }

    expanded
}

fn escape_argument(value: String) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if !value
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '\\' | ';' | '{' | '}' | '\'' | '"' | '#'))
    {
        return value;
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(contents: &str) -> String {
        filter_tmux_compat_input(&SourceInput {
            current_file: "~/.tmux.conf".to_owned(),
            contents: contents.to_owned(),
        })
        .contents
    }

    #[test]
    fn imports_static_options_and_key_unbindings() {
        let filtered = filter(
            "set -g status off\n\
             set -gw mode-style fg=black,bg=cyan\n\
             setw -g pane-border-style fg=colour3\n\
             bind-key -rn v split-window -h\n\
             unbind-key -nq C-b\n",
        );

        assert!(filtered.contains("set-option -g status off"));
        assert!(filtered.contains("set-option -g -w mode-style fg=black,bg=cyan"));
        assert!(filtered.contains("set-window-option -g pane-border-style fg=colour3"));
        assert!(!filtered.lines().any(|line| line.starts_with("bind-key ")));
        assert!(filtered.contains("unbind-key -n -q C-b"));
    }

    #[test]
    fn skips_shell_hooks_conditionals_and_recursive_sources() {
        let filtered = filter(
            "%if #{==:1,1}\n\
             set-hook -g after-new-window 'run-shell echo no'\n\
             run-shell '~/.tmux/plugins/tpm/tpm'\n\
             bind-key R source-file ~/.tmux.conf\n\
             bind-key X run-shell 'touch /tmp/nope'\n\
             set -g @plugin 'tmux-plugins/tpm'\n\
             %endif\n",
        );

        assert!(!filtered.contains("%if"));
        assert!(!filtered.contains("set-hook"));
        assert!(!filtered.contains("run-shell"));
        assert!(!filtered.contains("source-file"));
        assert!(!filtered.contains("@plugin"));
    }

    #[test]
    fn skips_entire_conditional_blocks_before_importing() {
        let filtered = filter(
            "set -g status on\n\
             %if '0'\n\
             set -g status off\n\
             %else\n\
             set -g mouse on\n\
             %endif\n\
             set -g bell-action any\n",
        );

        assert!(filtered.contains("set-option -g status on"));
        assert!(filtered.contains("set-option -g bell-action any"));
        assert!(!filtered.contains("status off"));
        assert!(!filtered.contains("mouse on"));
    }

    #[test]
    fn skips_nested_conditional_blocks_before_importing() {
        let filtered = filter(
            "%if '1'\n\
             set -g status off\n\
             %if '1'\n\
             set -g mouse on\n\
             %endif\n\
             %endif\n\
             set -g bell-action any\n",
        );

        assert_eq!(filtered, "set-option -g bell-action any\n");
    }

    #[test]
    fn skips_tmux_command_blocks_before_importing_inner_lines() {
        let filtered = filter(
            "if-shell 'test -f ~/.enable-rmux' {\n\
             set -g status off\n\
             set -g mouse on\n\
             }\n\
             set -g bell-action any\n",
        );

        assert_eq!(filtered, "set-option -g bell-action any\n");
    }

    #[test]
    fn skips_nested_tmux_command_blocks_before_importing_inner_lines() {
        let filtered = filter(
            "if -F '#{==:1,1}' {\n\
             if-shell 'true' {\n\
             set -g status off\n\
             }\n\
             set -g mouse on\n\
             }\n\
             set -g bell-action any\n",
        );

        assert_eq!(filtered, "set-option -g bell-action any\n");
    }

    #[test]
    fn braces_inside_quotes_do_not_start_tmux_command_blocks() {
        let filtered = filter(
            "run-shell 'echo {'\n\
             set -g status-left '{prefix}'\n\
             set -g status off\n",
        );

        assert_eq!(
            filtered,
            "set-option -g status-left '{prefix}'\nset-option -g status off\n"
        );
    }

    #[test]
    fn braces_inside_skipped_blocks_do_not_change_block_depth() {
        let filtered = filter(
            "if-shell 'true' {\n\
             display-message 'literal }'\n\
             set -g status off\n\
             }\n\
             set -g bell-action any\n",
        );

        assert_eq!(filtered, "set-option -g bell-action any\n");
    }

    #[test]
    fn skips_unknown_options_and_keeps_later_supported_options() {
        let filtered = filter(
            "set -g definitely-not-rmux yes\n\
             set -g status maybe-invalid-bool\n\
             set -g status on\n",
        );

        assert!(!filtered.contains("definitely-not-rmux"));
        assert!(!filtered.contains("maybe-invalid-bool"));
        assert!(filtered.contains("set-option -g status on"));
    }

    #[test]
    fn skips_shell_capable_options_from_tmux_conf() {
        let filtered = filter(
            "set -g default-shell /tmp/evil-shell\n\
             set -g default-command 'touch /tmp/nope; exec sh'\n\
             set -s default-client-command 'run-shell true'\n\
             set -s copy-command 'cat >/tmp/copy'\n\
             set -g lock-command 'touch /tmp/lock'\n\
             set -s command-alias[0] 'oops=run-shell true'\n\
             set -g editor 'sh -c evil'\n\
             set -g status on\n",
        );

        assert!(!filtered.contains("default-shell"));
        assert!(!filtered.contains("default-command"));
        assert!(!filtered.contains("default-client-command"));
        assert!(!filtered.contains("copy-command"));
        assert!(!filtered.contains("lock-command"));
        assert!(!filtered.contains("command-alias"));
        assert!(!filtered.contains("editor"));
        assert!(filtered.contains("set-option -g status on"));
    }

    #[test]
    fn skips_capability_clipboard_and_environment_options_from_tmux_conf() {
        let filtered = filter(
            "set -g allow-passthrough on\n\
             set -g get-clipboard on\n\
             set -g set-clipboard on\n\
             set -s terminal-features 'xterm*:clipboard:RGB'\n\
             set -s terminal-overrides 'xterm*:Ms=evil'\n\
             set -g update-environment 'SSH_AUTH_SOCK SECRET_TOKEN'\n\
             set -g status on\n",
        );

        assert!(!filtered.contains("allow-passthrough"));
        assert!(!filtered.contains("get-clipboard"));
        assert!(!filtered.contains("set-clipboard"));
        assert!(!filtered.contains("terminal-features"));
        assert!(!filtered.contains("terminal-overrides"));
        assert!(!filtered.contains("update-environment"));
        assert!(filtered.contains("set-option -g status on"));
    }

    #[test]
    fn skips_supported_options_that_are_not_tmux_fallback_allowlisted() {
        let filtered = filter(
            "set -g default-terminal tmux-256color\n\
             set -g history-file /tmp/rmux-history\n\
             set -s codepoint-widths U+1F9E0=2\n\
             set -s user-keys[0] '\\e[999~'\n\
             set -g status on\n",
        );

        assert!(!filtered.contains("default-terminal"));
        assert!(!filtered.contains("history-file"));
        assert!(!filtered.contains("codepoint-widths"));
        assert!(!filtered.contains("user-keys"));
        assert!(filtered.contains("set-option -g status on"));
    }

    #[test]
    fn skips_format_jobs_from_tmux_conf_values() {
        let filtered = filter(
            "set -g status-right '#(touch /tmp/nope)'\n\
             set -g status-left '#{session_name}'\n",
        );

        assert!(!filtered.contains("status-right"));
        assert!(filtered.contains("set-option -g status-left '#{session_name}'"));
    }

    #[test]
    fn skips_environment_mutations_from_tmux_conf() {
        let filtered = filter(
            "set-environment -g FOO bar\n\
             setenv -g SHELL /tmp/evil-shell\n\
             set-environment -g PATH /tmp/bin\n\
             set -g status on\n",
        );

        assert!(!filtered.contains("set-environment"));
        assert!(!filtered.contains("SHELL"));
        assert!(!filtered.contains("PATH"));
        assert!(filtered.contains("set-option -g status on"));
    }

    #[test]
    fn quotes_backslashes_when_reemitting_filtered_commands() {
        let filtered = filter("set -g status-left \"C:\\\\Users\\\\name\"\n");

        assert_eq!(filtered, "set-option -g status-left 'C:\\Users\\name'\n");
        CommandParser::new()
            .parse(&filtered)
            .expect("filtered backslash values should parse as rmux commands");
    }

    #[test]
    fn joins_simple_tmux_continuations_before_filtering() {
        let filtered = filter("set -g status-left hello\\\n-world\n");

        assert!(filtered.contains("set-option -g status-left hello-world"));
    }

    #[test]
    fn rejects_parse_time_assignments_and_unknown_commands() {
        assert!(filter("FOO=bar set -g status off\n").is_empty());
        assert!(filter("this-is-not-a-command\n").is_empty());
    }

    #[test]
    fn key_bindings_are_not_imported_from_tmux_conf() {
        assert!(filter("bind-key v splitw -h\n").is_empty());
        assert!(filter("bind-key n next-window\n").is_empty());
        assert!(filter("bind-key -T copy-mode-vi v send-keys -X begin-selection\n").is_empty());
        assert!(filter("bind-key e send-keys 'curl bad | sh' Enter\n").is_empty());
        assert!(filter("bind-key c new-window 'touch /tmp/nope'\n").is_empty());
        assert!(filter("bind-key % split-window 'touch /tmp/nope'\n").is_empty());
        assert!(filter("bind-key r run 'touch /tmp/nope'\n").is_empty());
        assert!(filter("bind-key -Z v split-window -h\n").is_empty());
        assert!(filter("bind-key\n").is_empty());
        assert!(filter("bind-key definitely-not-a-key split-window -h\n").is_empty());
        assert!(filter("bind-key x { split-window -h }\n").is_empty());
    }

    #[test]
    fn global_key_unbinds_are_not_imported_from_tmux_conf() {
        assert!(filter("unbind-key -a\n").is_empty());
        assert!(filter("unbind -a\n").is_empty());
        assert!(filter("unbind-key -aq\n").is_empty());
        assert!(filter("unbind-key -T copy-mode-vi -a\n").is_empty());

        let filtered = filter("unbind-key -a\nset -g status on\n");
        assert_eq!(filtered, "set-option -g status on\n");
    }

    #[test]
    fn direct_runtime_commands_are_not_imported_from_tmux_conf() {
        assert!(filter("new-session -d -s surprise\n").is_empty());
        assert!(filter("source-file ~/.tmux/plugins/tpm/tpm\n").is_empty());
        assert!(filter("if-shell 'test -f ~/.foo' 'set -g status off'\n").is_empty());
        assert!(filter("run-shell 'touch /tmp/nope'\n").is_empty());
        assert!(filter("kill-server\n").is_empty());
    }

    #[test]
    fn shell_capable_binding_commands_are_rejected() {
        assert!(filter("bind-key : command-prompt\n").is_empty());
        assert!(filter("bind-key x confirm-before kill-pane\n").is_empty());
        assert!(filter("bind-key p display-popup -E htop\n").is_empty());
        assert!(filter("bind-key P pipe-pane 'cat >/tmp/log'\n").is_empty());
    }

    #[test]
    fn command_lists_are_all_or_nothing() {
        let filtered = filter("set -g status off; setw -g pane-border-style fg=colour2\n");

        assert!(filtered.contains(
            "set-option -g status off ; set-window-option -g pane-border-style fg=colour2"
        ));
        assert!(filter("set -g status off; run-shell true\n").is_empty());
        assert!(filter("bind-key q split-window -h ; run-shell true\n").is_empty());
    }

    #[test]
    fn validates_option_scope_values_and_array_indexes_before_importing() {
        assert!(filter("set -s mode-style fg=red\n").is_empty());
        assert!(filter("set -g status-left[abc] value\n").is_empty());

        let filtered = filter("set -g status-format[0] '#[bold]#{session_name}'\n");
        assert!(filtered.contains("set-option -g status-format[0] '#[bold]#{session_name}'"));
    }

    #[test]
    fn filtered_configuration_roundtrips_to_regular_rmux_commands() {
        let filtered = filter(
            "set -g status off\n\
             set -g mouse on\n\
             setw -g mode-style fg=black,bg=cyan\n",
        );

        let parsed = CommandParser::new()
            .parse(&filtered)
            .expect("filtered tmux compat config should parse as rmux commands");
        assert_eq!(parsed.commands().len(), 3);
    }
}
