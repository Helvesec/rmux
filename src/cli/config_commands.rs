use std::path::Path;

#[path = "config_commands/hooks.rs"]
mod hooks;
#[path = "config_commands/options.rs"]
mod options;

use rmux_client::{connect, ClientError};
use rmux_proto::{
    ErrorResponse, Request, Response, RmuxError, ScopeSelector, SetEnvironmentMode,
    SetOptionByNameRequest,
};

use crate::cli::target_resolution::resolve_session_target_spec;
use crate::cli::{
    expect_command_output, expect_command_success, resolve_current_session_target,
    run_command_resolved, run_payload_command_resolved, write_command_output, ExitFailure,
};
use crate::cli_args::{
    SetEnvironmentArgs, SetOptionArgs, SetOptionCommandKind, ShowEnvironmentArgs, ShowOptionsArgs,
    ShowOptionsCommandKind, TargetSpec,
};
pub(crate) use hooks::{run_set_hook, run_show_hooks};
use options::{resolve_set_option_args, resolve_show_options_scope, ResolvedSetOptionCommand};

pub(crate) fn run_set_option(
    command: SetOptionCommandKind,
    args: SetOptionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let quiet = args.quiet;
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let request = match resolve_set_option_args(&mut connection, command, args) {
        Ok(ResolvedSetOptionCommand::NoOp) => return Ok(0),
        Ok(ResolvedSetOptionCommand::Request(request)) => request,
        Err(error) if quiet && quiet_option_failure(&error) => return Ok(0),
        Err(error) => return Err(error),
    };

    let response = connection
        .roundtrip(&Request::SetOptionByName(Box::new(
            SetOptionByNameRequest {
                scope: request.scope,
                name: request.option,
                value: request.value,
                mode: request.mode,
                only_if_unset: request.only_if_unset,
                unset: request.unset,
                unset_pane_overrides: request.unset_pane_overrides,
                format: request.format,
                format_target: request.format_target,
            },
        )))
        .map_err(ExitFailure::from_client)?;
    match response {
        response if quiet && quiet_option_response(&response) => Ok(0),
        response => {
            expect_command_success(response, command.command_name())?;
            Ok(0)
        }
    }
}

pub(crate) fn run_set_environment(
    args: SetEnvironmentArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mode = resolve_set_environment_mode(&args)?;
    if args.name.contains('=') {
        return Err(ExitFailure::new(1, "variable name contains ="));
    }
    let value = match mode {
        Some(SetEnvironmentMode::Clear | SetEnvironmentMode::Unset) => String::new(),
        Some(SetEnvironmentMode::Set) | None => args
            .value
            .clone()
            .ok_or_else(|| ExitFailure::new(1, "no value specified"))?,
    };

    run_command_resolved(socket_path, "set-environment", move |connection| {
        let scope = resolve_environment_scope(connection, args.global, args.target)?;
        connection
            .set_environment(scope, args.name, value, mode, args.hidden, args.format)
            .map_err(ExitFailure::from_client)
    })
}

pub(crate) fn run_show_options(
    command: ShowOptionsCommandKind,
    args: ShowOptionsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let command_name = command.command_name();
    let quiet = args.quiet;
    let scope = match resolve_show_options_scope(command, &args) {
        Ok(scope) => scope,
        Err(error) if quiet && quiet_option_failure(&error) => return Ok(0),
        Err(error) => return Err(error),
    };

    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let scope = match scope.resolve(&mut connection, command_name) {
        Ok(scope) => scope,
        Err(error) if quiet && quiet_option_failure(&error) => return Ok(0),
        Err(error) => return Err(error),
    };
    let include_inherited = args.include_inherited;
    let include_hooks = args.include_hooks;
    let response = connection
        .show_options_extended(
            scope,
            args.name,
            args.value_only,
            include_inherited,
            args.quiet,
            include_hooks,
        )
        .map_err(show_options_exit_failure)?;
    match response {
        response if quiet && quiet_option_response(&response) => Ok(0),
        Response::Error(ErrorResponse {
            error: RmuxError::Server(message) | RmuxError::Message(message),
        }) => Err(show_options_message_failure(message)),
        response => {
            let output = expect_command_output(&response, command_name)?;
            write_command_output(output)?;
            Ok(0)
        }
    }
}

fn show_options_exit_failure(error: ClientError) -> ExitFailure {
    match error {
        ClientError::Protocol(RmuxError::Server(message) | RmuxError::Message(message)) => {
            let normalized = message
                .strip_prefix("server error: ")
                .unwrap_or(&message)
                .to_owned();
            if option_lookup_error(&normalized) {
                ExitFailure::new(1, normalized)
            } else {
                ExitFailure::from_client(ClientError::Protocol(RmuxError::Server(message)))
            }
        }
        error => ExitFailure::from_client(error),
    }
}

fn show_options_message_failure(message: String) -> ExitFailure {
    let normalized = message
        .strip_prefix("server error: ")
        .unwrap_or(&message)
        .to_owned();
    if option_lookup_error(&normalized) {
        ExitFailure::new(1, normalized)
    } else {
        ExitFailure::new(1, message)
    }
}

fn quiet_option_failure(error: &ExitFailure) -> bool {
    let message = error.message();
    message.starts_with("invalid option: ")
        || message.starts_with("server error: unknown option: ")
        || message.starts_with("server error: invalid option: ")
        || message.starts_with("server error: ambiguous option: ")
}

fn quiet_option_response(response: &Response) -> bool {
    matches!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::Server(message),
        }) if option_lookup_error(message)
    )
}

fn option_lookup_error(message: &str) -> bool {
    message.starts_with("unknown option: ")
        || message.starts_with("invalid option: ")
        || message.starts_with("ambiguous option: ")
}

pub(crate) fn run_show_environment(
    args: ShowEnvironmentArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_payload_command_resolved(socket_path, "show-environment", move |connection| {
        let scope = resolve_environment_scope(connection, args.global, args.target)?;
        connection
            .show_environment(scope, args.name, args.hidden, args.shell_format)
            .map_err(ExitFailure::from_client)
    })
}

fn resolve_environment_scope(
    connection: &mut rmux_client::Connection,
    global: bool,
    target: Option<TargetSpec>,
) -> Result<ScopeSelector, ExitFailure> {
    if global {
        return Ok(ScopeSelector::Global);
    }
    match target {
        Some(target) => {
            resolve_session_target_spec(connection, &target, false).map(ScopeSelector::Session)
        }
        None => resolve_current_session_target(connection).map(ScopeSelector::Session),
    }
}

fn resolve_set_environment_mode(
    args: &SetEnvironmentArgs,
) -> Result<Option<SetEnvironmentMode>, ExitFailure> {
    let mode = match (args.clear, args.unset) {
        (true, false) => Some(SetEnvironmentMode::Clear),
        (false, true) => Some(SetEnvironmentMode::Unset),
        (false, false) => Some(SetEnvironmentMode::Set),
        (true, true) => {
            return Err(ExitFailure::new(
                1,
                "set-environment accepts at most one of -r or -u",
            ))
        }
    };

    if matches!(
        mode,
        Some(SetEnvironmentMode::Clear | SetEnvironmentMode::Unset)
    ) && args.value.is_some()
    {
        return Err(ExitFailure::new(
            1,
            "set-environment -r and -u do not accept a value",
        ));
    }

    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::{
        options::{
            resolve_set_option_args_with_exact_targets as resolve_set_option_command,
            ResolvedSetOptionArgs, ResolvedSetOptionCommand, ShowOptionsScope,
            UnresolvedShowOptionsScope,
        },
        resolve_show_options_scope,
    };
    use crate::cli_args::{
        parse_target_spec, SetEnvironmentArgs, SetOptionArgs, SetOptionCommandKind,
        ShowOptionsArgs, ShowOptionsCommandKind, TargetSpec,
    };
    use rmux_proto::{OptionScopeSelector, PaneTarget, SessionName, WindowTarget};
    use std::path::Path;

    fn target_spec(value: &str) -> TargetSpec {
        parse_target_spec(value).expect("valid target spec")
    }

    fn global_set_args(option: &str, value: &str) -> SetOptionArgs {
        SetOptionArgs {
            global: true,
            server: false,
            window: false,
            pane: false,
            quiet: false,
            append: false,
            format: false,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            target: None,
            option: option.to_owned(),
            value: Some(value.to_owned()),
        }
    }

    fn show_global_args(name: Option<&str>) -> ShowOptionsArgs {
        ShowOptionsArgs {
            include_inherited: false,
            include_hooks: false,
            global: true,
            server: false,
            window: false,
            pane: false,
            quiet: false,
            value_only: false,
            target: None,
            name: name.map(str::to_owned),
        }
    }

    fn resolve_set_option_args(
        command: SetOptionCommandKind,
        args: SetOptionArgs,
    ) -> Result<ResolvedSetOptionArgs, crate::cli::ExitFailure> {
        match resolve_set_option_command(command, args)? {
            ResolvedSetOptionCommand::Request(request) => Ok(request),
            ResolvedSetOptionCommand::NoOp => {
                panic!("expected set-option to resolve to a request")
            }
        }
    }

    #[test]
    fn set_environment_without_value_reports_tmux_message_before_connecting() {
        let error = super::run_set_environment(
            SetEnvironmentArgs {
                global: false,
                target: None,
                format: false,
                hidden: false,
                clear: false,
                unset: false,
                name: "FOO".to_owned(),
                value: None,
            },
            Path::new("/tmp/rmux-setenv-value-test.sock"),
        )
        .expect_err("missing set-environment value should fail before connecting");

        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.message(), "no value specified");
    }

    #[test]
    fn set_environment_rejects_equals_in_name_before_value_validation() {
        let error = super::run_set_environment(
            SetEnvironmentArgs {
                global: true,
                target: None,
                format: false,
                hidden: false,
                clear: false,
                unset: false,
                name: "FOO=bar".to_owned(),
                value: None,
            },
            Path::new("/tmp/rmux-setenv-equals-test.sock"),
        )
        .expect_err("set-environment names containing equals should fail before connecting");

        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.message(), "variable name contains =");
    }

    #[test]
    fn set_window_option_uses_window_scope_for_window_targets() {
        let session = SessionName::new("alpha").expect("valid session");
        let window = WindowTarget::with_window(session, 0);
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetWindowOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(target_spec("alpha:0")),
                option: "pane-border-style".to_owned(),
                value: Some("fg=colour1".to_owned()),
            },
        )
        .expect("window-scoped set-window-option resolves");

        assert_eq!(resolved.scope, OptionScopeSelector::Window(window));
    }

    #[test]
    fn set_option_unset_pane_overrides_implies_unset_at_session_scope() {
        // Oracle probe 2026-07-09: plain `set -U` unsets the session copy
        // only; the pane-override sweep applies only when -w selects a
        // window scope.
        let session = SessionName::new("alpha").expect("valid session");
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: true,
                target: Some(target_spec("alpha:0.1")),
                option: "@agent.state".to_owned(),
                value: None,
            },
        )
        .expect("set-option -U resolves without requiring a value");

        assert_eq!(resolved.scope, OptionScopeSelector::Session(session));
        assert!(resolved.unset);
        assert!(resolved.unset_pane_overrides);
        assert_eq!(resolved.value, None);
    }

    #[test]
    fn set_option_global_flag_uses_the_named_option_global_root() {
        for (option, value, expected) in [
            ("message-limit", "77", OptionScopeSelector::ServerGlobal),
            ("status", "off", OptionScopeSelector::SessionGlobal),
            (
                "mode-style",
                "fg=black,bg=red",
                OptionScopeSelector::WindowGlobal,
            ),
            (
                "copy-mode-selection-style",
                "fg=black,bg=cyan",
                OptionScopeSelector::WindowGlobal,
            ),
        ] {
            let resolved = resolve_set_option_args(
                SetOptionCommandKind::SetOption,
                global_set_args(option, value),
            )
            .expect("global set-option resolves");

            assert_eq!(resolved.scope, expected, "{option} should choose its root");
        }
    }

    #[test]
    fn set_option_server_flag_is_ignored_for_non_server_options_like_tmux() {
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                server: true,
                ..global_set_args("mode-style", "fg=black,bg=red")
            },
        )
        .expect("window global set-option resolves");
        assert_eq!(resolved.scope, OptionScopeSelector::WindowGlobal);

        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                global: false,
                server: true,
                append: true,
                target: Some(target_spec("alpha")),
                option: "status-left".to_owned(),
                value: Some("append".to_owned()),
                ..global_set_args("status-left", "append")
            },
        )
        .expect("targeted session set-option resolves");
        assert_eq!(
            resolved.scope,
            OptionScopeSelector::Session(SessionName::new("alpha").expect("valid session name"))
        );
    }

    #[test]
    fn set_option_explicit_global_window_scope_still_wins() {
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                window: true,
                ..global_set_args("copy-mode-selection-style", "fg=black,bg=cyan")
            },
        )
        .expect("set-option -gw resolves");

        assert_eq!(resolved.scope, OptionScopeSelector::WindowGlobal);
    }

    #[test]
    fn set_option_global_with_scope_flag_uses_named_option_global_root() {
        for (server, window, pane) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let resolved = resolve_set_option_args(
                SetOptionCommandKind::SetOption,
                SetOptionArgs {
                    server,
                    window,
                    pane,
                    ..global_set_args("status", "off")
                },
            )
            .expect("set-option -g plus scope flag resolves");

            assert_eq!(resolved.scope, OptionScopeSelector::SessionGlobal);
        }
    }

    #[test]
    fn set_option_explicit_pane_scope_wins_for_pane_capable_known_options() {
        let session = SessionName::new("alpha").expect("valid session");
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: true,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(target_spec("alpha:0.1")),
                option: "window-style".to_owned(),
                value: Some("bg=red".to_owned()),
            },
        )
        .expect("set-option -p resolves to a pane scope");

        assert_eq!(
            resolved.scope,
            OptionScopeSelector::Pane(PaneTarget::with_window(session, 0, 1))
        );
    }

    #[test]
    fn set_window_option_uses_natural_scope_for_session_options() {
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetWindowOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(target_spec("alpha")),
                option: "status".to_owned(),
                value: Some("off".to_owned()),
            },
        )
        .expect("set-window-option session option resolves");

        assert_eq!(
            resolved.scope,
            OptionScopeSelector::Session(SessionName::new("alpha").expect("valid session"))
        );

        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetWindowOption,
            SetOptionArgs {
                ..global_set_args("history-limit", "1234")
            },
        )
        .expect("set-window-option -g session option resolves");

        assert_eq!(resolved.scope, OptionScopeSelector::SessionGlobal);
    }

    #[test]
    fn set_window_option_uses_current_window_for_session_targets() {
        let session = SessionName::new("alpha").expect("valid session");
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetWindowOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(target_spec("alpha")),
                option: "pane-border-style".to_owned(),
                value: Some("fg=colour1".to_owned()),
            },
        )
        .expect("session-target set-window-option resolves");

        assert_eq!(
            resolved.scope,
            OptionScopeSelector::Window(WindowTarget::new(session))
        );
    }

    #[test]
    fn set_option_infers_window_scope_for_session_targets_when_option_is_window_scoped() {
        let session = SessionName::new("alpha").expect("valid session");
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(target_spec("alpha")),
                option: "remain-on-exit".to_owned(),
                value: Some("on".to_owned()),
            },
        )
        .expect("session-target set-option should infer the current window scope");

        assert_eq!(
            resolved.scope,
            OptionScopeSelector::Window(WindowTarget::new(session))
        );
    }

    #[test]
    fn set_window_option_uses_window_scope_for_pane_targets() {
        let session = SessionName::new("alpha").expect("valid session");
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetWindowOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(target_spec("alpha:0.1")),
                option: "pane-border-style".to_owned(),
                value: Some("fg=colour1".to_owned()),
            },
        )
        .expect("pane-target set-window-option resolves");

        assert_eq!(
            resolved.scope,
            OptionScopeSelector::Window(WindowTarget::with_window(session, 0))
        );
    }

    #[test]
    fn show_options_global_flag_uses_the_named_option_global_root() {
        for (name, expected) in [
            ("message-limit", OptionScopeSelector::ServerGlobal),
            ("status", OptionScopeSelector::SessionGlobal),
            ("mode-style", OptionScopeSelector::WindowGlobal),
            (
                "copy-mode-selection-style",
                OptionScopeSelector::WindowGlobal,
            ),
        ] {
            let scope = resolve_show_options_scope(
                ShowOptionsCommandKind::ShowOptions,
                &show_global_args(Some(name)),
            )
            .expect("show-options -g resolves");

            assert_eq!(
                scope,
                ShowOptionsScope::Resolved(expected),
                "{name} should show from its global option tree"
            );
        }
    }

    #[test]
    fn show_options_global_flag_without_name_keeps_session_global_default() {
        let scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowOptions,
            &show_global_args(None),
        )
        .expect("show-options -g resolves");

        assert_eq!(
            scope,
            ShowOptionsScope::Resolved(OptionScopeSelector::SessionGlobal)
        );
    }

    #[test]
    fn show_options_window_and_pane_flags_without_target_use_current_target() {
        let window_scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowOptions,
            &ShowOptionsArgs {
                global: false,
                window: true,
                ..show_global_args(Some("@missing"))
            },
        )
        .expect("show-options -w resolves");
        assert_eq!(window_scope, ShowOptionsScope::CurrentWindow);

        let pane_scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowOptions,
            &ShowOptionsArgs {
                global: false,
                pane: true,
                ..show_global_args(Some("@missing"))
            },
        )
        .expect("show-options -p resolves");
        assert_eq!(pane_scope, ShowOptionsScope::CurrentPane);
    }

    #[test]
    fn show_window_options_accepts_window_targets_without_server_scope() {
        let scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowWindowOptions,
            &ShowOptionsArgs {
                include_inherited: false,
                include_hooks: false,
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                value_only: true,
                target: Some(target_spec("alpha:0")),
                name: Some("pane-border-style".to_owned()),
            },
        )
        .expect("window-target show-window-options resolves");

        assert_eq!(
            scope,
            ShowOptionsScope::Unresolved {
                target: target_spec("alpha:0"),
                kind: UnresolvedShowOptionsScope::Window,
            }
        );
    }

    #[test]
    fn show_window_options_uses_window_global_scope_with_g_flag() {
        let scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowWindowOptions,
            &ShowOptionsArgs {
                include_inherited: false,
                include_hooks: false,
                global: true,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                value_only: false,
                target: None,
                name: None,
            },
        )
        .expect("show-window-options -g resolves");

        assert_eq!(
            scope,
            ShowOptionsScope::Resolved(OptionScopeSelector::WindowGlobal)
        );
    }

    #[test]
    fn show_options_accepts_combined_global_and_server_flags_with_target_compatibility() {
        let scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowOptions,
            &ShowOptionsArgs {
                include_inherited: false,
                include_hooks: false,
                global: true,
                server: true,
                window: false,
                pane: false,
                quiet: false,
                value_only: true,
                target: Some(target_spec("missing")),
                name: Some("message-limit".to_owned()),
            },
        )
        .expect("show-options -gsv -t resolves");

        assert_eq!(
            scope,
            ShowOptionsScope::Resolved(OptionScopeSelector::ServerGlobal)
        );
    }

    #[test]
    fn show_window_options_global_scope_ignores_target_compatibility_argument() {
        let scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowWindowOptions,
            &ShowOptionsArgs {
                include_inherited: false,
                include_hooks: false,
                global: true,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                value_only: true,
                target: Some(target_spec("missing")),
                name: Some("pane-border-style".to_owned()),
            },
        )
        .expect("show-window-options -g -t resolves");

        assert_eq!(
            scope,
            ShowOptionsScope::Resolved(OptionScopeSelector::WindowGlobal)
        );
    }

    #[test]
    fn set_option_reports_invalid_option_before_scope_errors() {
        let result = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                global: true,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                append: false,
                format: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: None,
                option: "nonexistent".to_owned(),
                value: Some("value".to_owned()),
            },
        );
        let error = match result {
            Ok(_) => panic!("unknown option should fail"),
            Err(error) => error,
        };

        assert_eq!(error.message(), "invalid option: nonexistent");
    }
}
