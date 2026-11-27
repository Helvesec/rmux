use rmux_core::command_parser::{
    check_tmux_arguments, CommandArgument, CommandParser, ParsedCommand, ParsedCommands,
};
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
    parser: &CommandParser,
) -> Result<SetHookMutationRequest, RmuxError> {
    let Some(command) = request.command.as_deref() else {
        return Ok(request);
    };

    match parser.parse(command) {
        Ok(parsed) => {
            validate_parsed_hook_commands(&parsed)?;
            request.command = Some(hook_command_list_string(&parsed));
            Ok(request)
        }
        Err(error) => Err(RmuxError::Server(error.to_string())),
    }
}

/// Renders a hook's command list the way tmux `cmd_list_print` shows it and
/// so that replaying the stored text yields the same commands.
///
/// tmux keeps a hook as a parsed command list and runs every command in it.
/// The bind-key argv form (` \; `) must not be stored here: the hook replay
/// parses the stored text as a command string, where `\;` is a literal `;`
/// argument, so the whole list would collapse into one invalid command. All
/// commands are joined with ` ; `, whatever line they came from, because
/// tmux shows them that way and stops the whole hook list on a failure.
fn hook_command_list_string(commands: &ParsedCommands) -> String {
    commands
        .commands()
        .iter()
        .map(ParsedCommand::to_tmux_reparse_string)
        .collect::<Vec<_>>()
        .join(" ; ")
}

fn validate_parsed_hook_commands(commands: &ParsedCommands) -> Result<(), RmuxError> {
    for command in commands.commands() {
        // tmux builds brace blocks before checking their containing command.
        // Strings stay deferred until the command that owns them parses them.
        for argument in command.arguments() {
            if let CommandArgument::Commands(nested) = argument {
                validate_parsed_hook_commands(nested)?;
            }
        }
        check_tmux_arguments(command).map_err(|error| RmuxError::Server(error.to_string()))?;
    }
    Ok(())
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

        let canonical = super::canonicalize_set_hook_mutation_command(
            request,
            &rmux_core::command_parser::CommandParser::new(),
        )
        .expect("known commands should canonicalize");
        assert_eq!(
            canonical.command.as_deref(),
            Some("display-message hi ; select-window -t :0")
        );
    }

    #[test]
    fn stores_a_hook_list_as_a_replayable_tmux_command_list() {
        let parser = rmux_core::command_parser::CommandParser::new();
        for source in [
            "set-option -g @a 1 ; set-option -g @b 'x;y'",
            "set-option -g @a 1\nset-option -g @b 'x;y'",
        ] {
            let request = SetHookMutationRequest {
                scope: ScopeSelector::Global,
                hook: HookName::AfterNewWindow,
                command: Some(source.to_owned()),
                lifecycle: HookLifecycle::Persistent,
                append: false,
                unset: false,
                run_immediately: false,
                index: None,
            };

            let stored = super::canonicalize_set_hook_mutation_command(request, &parser)
                .expect("a hook list should canonicalize")
                .command
                .expect("the hook keeps its command");

            assert_eq!(stored, "set-option -g @a 1 ; set-option -g @b \"x;y\"");
            let replayed = parser.parse(&stored).expect("stored hook reparses");
            let names: Vec<_> = replayed.commands().iter().map(|c| c.name()).collect();
            assert_eq!(names, ["set-option", "set-option"], "{source:?}");
        }
    }

    #[test]
    fn canonicalizes_user_command_aliases_before_hook_storage() {
        let request = SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterSetBuffer,
            command: Some("sbuf from-hook".to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        };
        let parser = rmux_core::command_parser::CommandParser::new()
            .with_command_alias("sbuf=set-buffer -b aliased")
            .expect("valid command alias");

        let canonical = super::canonicalize_set_hook_mutation_command(request, &parser)
            .expect("user aliases should canonicalize");

        assert_eq!(
            canonical.command.as_deref(),
            Some("set-buffer -b aliased from-hook")
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

        let error = super::canonicalize_set_hook_mutation_command(
            request,
            &rmux_core::command_parser::CommandParser::new(),
        )
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

        let error = super::canonicalize_set_hook_mutation_command(
            request,
            &rmux_core::command_parser::CommandParser::new(),
        )
        .expect_err("unknown hook commands must be rejected");
        assert!(
            error.to_string().contains("unknown command: printf"),
            "unexpected error: {error}"
        );
    }

    // Measured against tmux 3.7b on Darwin and Linux: parse errors reject the
    // complete hook list, including brace blocks, before storage or execution.
    #[test]
    fn hook_args_parse_rejects_invalid_commands_everywhere_in_the_list() {
        for (command, cause) in [
            (
                r"set-option -g @a 1 \; set-option -g @b 2",
                "command set-option: too many arguments (need at most 2)",
            ),
            (
                "set-option -g @a 1 ; display-message -Q",
                "command display-message: unknown flag -Q",
            ),
            (
                "set-option -g",
                "command set-option: too few arguments (need at least 1)",
            ),
            (
                "display-message -t",
                "command display-message: -t expects an argument",
            ),
            (
                "display-message -:",
                "command display-message: invalid flag -:",
            ),
            (
                "if-shell -F 1 -Z { display-message -Q }",
                "command display-message: unknown flag -Q",
            ),
            (
                "display-message -t { display-message hi }",
                "command display-message: -t argument must be a string",
            ),
            (
                "display-message hi -Q",
                "command display-message: too many arguments (need at most 1)",
            ),
        ] {
            let error = canonicalize_hook(command).expect_err(command);
            assert_eq!(
                error,
                rmux_proto::RmuxError::Server(cause.to_owned()),
                "{command}"
            );
        }
    }

    #[test]
    fn hook_args_parse_preserves_valid_lists_and_deferred_strings() {
        for command in [
            "next-window -at:99",
            "set-option -g @a 1 ; set-option -g @b 2",
            "set-option -g @a 'x;y'",
            "display-message -- -Q",
            "if-shell -F 1 'display-message -Q'",
            "if-shell -F 1 { display-message hi ; set-option -g @b 2 }",
        ] {
            canonicalize_hook(command).expect(command);
        }
    }

    #[tokio::test]
    async fn hook_args_parse_rejects_both_request_paths_without_mutating_the_hook() {
        use rmux_proto::{Request, Response, ShowHooksRequest};

        let handler = crate::handler::RequestHandler::new();
        let valid = canonicalize_hook("display-message original").expect("valid hook");
        assert!(matches!(
            handler
                .handle(Request::SetHookMutation(valid.clone()))
                .await,
            Response::SetHook(_)
        ));
        let show = Request::ShowHooks(ShowHooksRequest {
            scope: ScopeSelector::Global,
            window: false,
            pane: false,
            hook: Some(HookName::AfterNewWindow),
        });
        let before = handler.handle(show.clone()).await;
        let invalid = "display-message first ; display-message -Q";
        let legacy = Request::SetHook(SetHookRequest {
            scope: valid.scope.clone(),
            hook: valid.hook,
            command: invalid.to_owned(),
            lifecycle: valid.lifecycle,
        });
        let mut requests = vec![legacy];
        for (append, index) in [(false, None), (true, None), (false, Some(2))] {
            requests.push(Request::SetHookMutation(SetHookMutationRequest {
                command: Some(invalid.to_owned()),
                append,
                index,
                ..valid.clone()
            }));
        }
        for request in requests {
            let response = handler.handle(request).await;
            let Response::Error(error) = response else {
                panic!("invalid hook accepted: {response:?}");
            };
            assert_eq!(
                error.error,
                rmux_proto::RmuxError::Server(
                    "command display-message: unknown flag -Q".to_owned()
                )
            );
            assert_eq!(handler.handle(show.clone()).await, before);
        }
    }

    fn canonicalize_hook(command: &str) -> Result<SetHookMutationRequest, rmux_proto::RmuxError> {
        super::canonicalize_set_hook_mutation_command(
            SetHookMutationRequest {
                scope: ScopeSelector::Global,
                hook: HookName::AfterNewWindow,
                command: Some(command.to_owned()),
                lifecycle: HookLifecycle::Persistent,
                append: false,
                unset: false,
                run_immediately: false,
                index: None,
            },
            &rmux_core::command_parser::CommandParser::new(),
        )
    }

    fn shell_quote_str(value: &str) -> String {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}
