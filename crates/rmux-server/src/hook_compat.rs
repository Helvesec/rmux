use rmux_proto::{HookLifecycle, HookName, ScopeSelector, SetHookMutationRequest, SetHookRequest};

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
        command: shell_command,
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
        command: Some(shell_command),
        ..request
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
            "mkdir -p '/tmp/example' && printf 'attached\\n' > '/tmp/example/hook'"
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
        assert_eq!(normalized.command.as_deref(), Some("printf attached"));
    }

    fn shell_quote_str(value: &str) -> String {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}
