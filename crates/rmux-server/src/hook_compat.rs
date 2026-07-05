use rmux_core::command_parser::{CommandParser, ParsedCommand};
use rmux_proto::{
    HookLifecycle, HookName, RmuxError, ScopeSelector, SetHookMutationRequest, SetHookRequest,
};

pub(crate) fn normalize_set_hook_request(request: SetHookRequest) -> SetHookRequest {
    let Some(shell_command) = extract_self_unsetting_shell_command(
        &request.scope,
        request.hook,
        Some(request.command.as_str()),
        request.lifecycle,
    ) else {
        return request;
    };

    SetHookRequest {
        lifecycle: HookLifecycle::OneShot,
        command: run_shell_hook_command(&shell_command),
        ..request
    }
}

pub(crate) fn normalize_set_hook_mutation_request(
    request: SetHookMutationRequest,
) -> SetHookMutationRequest {
    if request.append || request.unset || request.run_immediately || request.index.is_some() {
        return request;
    }

    let Some(shell_command) = extract_self_unsetting_shell_command(
        &request.scope,
        request.hook,
        request.command.as_deref(),
        request.lifecycle,
    ) else {
        return request;
    };

    SetHookMutationRequest {
        lifecycle: HookLifecycle::OneShot,
        command: Some(run_shell_hook_command(&shell_command)),
        ..request
    }
}

pub(crate) fn canonicalize_set_hook_mutation_command(
    mut request: SetHookMutationRequest,
) -> Result<SetHookMutationRequest, RmuxError> {
    let Some(command) = request.command.as_deref() else {
        return Ok(request);
    };

    match CommandParser::new().parse(command) {
        Ok(parsed) => {
            validate_parsed_hook_commands(&parsed)?;
            request.command = Some(parsed.to_tmux_binding_string());
            Ok(request)
        }
        Err(error) => Err(RmuxError::Server(error.to_string())),
    }
}

fn validate_parsed_hook_commands(
    commands: &rmux_core::command_parser::ParsedCommands,
) -> Result<(), RmuxError> {
    for command in commands.commands() {
        validate_no_positional_hook_command(command)?;
    }
    Ok(())
}

fn validate_no_positional_hook_command(command: &ParsedCommand) -> Result<(), RmuxError> {
    let Some(spec) = NoPositionalCommandSpec::for_command(command.name()) else {
        return Ok(());
    };

    let mut index = 0;
    while index < command.arguments().len() {
        let argument = &command.arguments()[index];
        let Some(value) = argument.as_string() else {
            return Err(too_many_hook_arguments(
                command.name(),
                spec.max_positionals,
            ));
        };

        if value == "--" {
            if index + 1 < command.arguments().len() {
                return Err(too_many_hook_arguments(
                    command.name(),
                    spec.max_positionals,
                ));
            }
            return Ok(());
        }

        if spec.boolean_flags.contains(&value) {
            index += 1;
            continue;
        }

        if spec.value_flags.contains(&value) {
            index += 2;
            continue;
        }

        if spec
            .value_flags
            .iter()
            .any(|flag| value.starts_with(flag) && value.len() > flag.len())
        {
            index += 1;
            continue;
        }

        if value.starts_with('-') {
            return Err(RmuxError::Server(format!(
                "command {}: unknown flag {}",
                command.name(),
                value
            )));
        }

        return Err(too_many_hook_arguments(
            command.name(),
            spec.max_positionals,
        ));
    }

    Ok(())
}

fn too_many_hook_arguments(command: &str, max_positionals: usize) -> RmuxError {
    RmuxError::Server(format!(
        "command {command}: too many arguments (need at most {max_positionals})"
    ))
}

#[derive(Debug, Clone, Copy)]
struct NoPositionalCommandSpec {
    max_positionals: usize,
    boolean_flags: &'static [&'static str],
    value_flags: &'static [&'static str],
}

impl NoPositionalCommandSpec {
    fn for_command(command: &str) -> Option<Self> {
        match command {
            "last-window" | "next-layout" | "previous-layout" => Some(Self {
                max_positionals: 0,
                boolean_flags: &[],
                value_flags: &["-t"],
            }),
            "next-window" | "previous-window" => Some(Self {
                max_positionals: 0,
                boolean_flags: &["-a"],
                value_flags: &["-t"],
            }),
            _ => None,
        }
    }
}

fn extract_self_unsetting_shell_command(
    scope: &ScopeSelector,
    hook: HookName,
    command: Option<&str>,
    lifecycle: HookLifecycle,
) -> Option<String> {
    if lifecycle != HookLifecycle::Persistent || hook != HookName::ClientAttached {
        return None;
    }

    let ScopeSelector::Session(session_name) = scope else {
        return None;
    };

    let command = command?.trim();
    let run_shell = command.strip_prefix("run-shell")?;
    if run_shell.len() == command.len() {
        return None;
    }

    let (shell_command, remainder) = parse_single_quoted_shell_argument(run_shell.trim_start())?;
    let remainder = remainder.trim_start();
    let remainder = remainder.strip_prefix(';')?.trim();

    let tokens: Vec<&str> = remainder.split_whitespace().collect();
    if tokens
        == [
            "set-hook",
            "-u",
            "-t",
            session_name.as_str(),
            "client-attached",
        ]
    {
        Some(shell_command)
    } else {
        None
    }
}

fn parse_single_quoted_shell_argument(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if first != '\'' {
        return None;
    }

    let mut decoded = String::new();
    let mut cursor = 1;

    while cursor <= input.len() {
        let rest = &input[cursor..];
        let closing = rest.find('\'')?;
        decoded.push_str(&rest[..closing]);
        cursor += closing + 1;

        let tail = &input[cursor..];
        if let Some(reopened) = tail.strip_prefix("\\''") {
            decoded.push('\'');
            cursor = input.len() - reopened.len();
            continue;
        }

        return Some((decoded, tail));
    }

    None
}

fn run_shell_hook_command(shell_command: &str) -> String {
    format!("run-shell {}", quote_tmux_single_argument(shell_command))
}

fn quote_tmux_single_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::normalize_set_hook_request;
    use rmux_proto::{
        HookLifecycle, HookName, ScopeSelector, SessionName, SetHookMutationRequest, SetHookRequest,
    };

    #[test]
    fn normalizes_self_unsetting_self_unsetting_hook_payloads() {
        let request = SetHookRequest {
            scope: ScopeSelector::Session(SessionName::new("alpha").expect("valid session name")),
            hook: HookName::ClientAttached,
            command: format!(
                "run-shell {}; set-hook -u -t alpha client-attached",
                shell_quote_str(
                    "mkdir -p '/tmp/example' && printf 'attached\\n' > '/tmp/example/hook'"
                )
            ),
            lifecycle: HookLifecycle::Persistent,
        };

        let normalized = normalize_set_hook_request(request);
        assert_eq!(normalized.lifecycle, HookLifecycle::OneShot);
        assert_eq!(
            normalized.command,
            "run-shell 'mkdir -p '\\''/tmp/example'\\'' && printf '\\''attached\\n'\\'' > '\\''/tmp/example/hook'\\'''"
        );
    }

    #[test]
    fn leaves_plain_shell_hooks_unchanged() {
        let request = SetHookRequest {
            scope: ScopeSelector::Session(SessionName::new("alpha").expect("valid session name")),
            hook: HookName::ClientAttached,
            command: "printf attached".to_owned(),
            lifecycle: HookLifecycle::Persistent,
        };

        let normalized = normalize_set_hook_request(request.clone());
        assert_eq!(normalized, request);
    }

    #[test]
    fn ignores_self_unsetting_payloads_for_other_sessions() {
        let request = SetHookRequest {
            scope: ScopeSelector::Session(SessionName::new("alpha").expect("valid session name")),
            hook: HookName::ClientAttached,
            command: format!(
                "run-shell {}; set-hook -u -t beta client-attached",
                shell_quote_str("printf attached")
            ),
            lifecycle: HookLifecycle::Persistent,
        };

        let normalized = normalize_set_hook_request(request.clone());
        assert_eq!(normalized, request);
    }

    #[test]
    fn mutation_requests_normalize_the_same_self_unsetting_payload_shape() {
        let request = SetHookMutationRequest {
            scope: ScopeSelector::Session(SessionName::new("alpha").expect("valid session name")),
            hook: HookName::ClientAttached,
            command: Some(format!(
                "run-shell {}; set-hook -u -t alpha client-attached",
                shell_quote_str("printf attached")
            )),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        };

        let normalized = super::normalize_set_hook_mutation_request(request);
        assert_eq!(normalized.lifecycle, HookLifecycle::OneShot);
        assert_eq!(
            normalized.command.as_deref(),
            Some("run-shell 'printf attached'")
        );
    }

    #[test]
    fn canonicalizes_known_hook_commands_before_storage() {
        let request = SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::PaneExited,
            command: Some("display hi ; selectw -t :0".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        };

        let canonical = super::canonicalize_set_hook_mutation_command(request)
            .expect("known commands should canonicalize");
        assert_eq!(
            canonical.command.as_deref(),
            Some("display-message hi \\; select-window -t :0")
        );
    }

    #[test]
    fn rejects_hook_commands_with_invalid_known_command_arity() {
        let request = SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::PaneExited,
            command: Some("next \"win\"".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        };

        let error = super::canonicalize_set_hook_mutation_command(request)
            .expect_err("known commands with invalid arity must be rejected");
        assert!(
            error.to_string().contains("too many arguments"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_unknown_hook_commands_at_registration_time() {
        let request = SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::PaneExited,
            command: Some("printf attached".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        };

        let error = super::canonicalize_set_hook_mutation_command(request)
            .expect_err("unknown hook commands must be rejected");
        assert!(
            error.to_string().contains("unknown command: printf"),
            "unexpected error: {error}"
        );
    }

    fn shell_quote_str(value: &str) -> String {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}
