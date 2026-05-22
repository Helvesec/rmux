use std::path::Path;

#[path = "config_commands/options.rs"]
mod options;

use rmux_proto::{HookLifecycle, ScopeSelector, SetEnvironmentMode, Target};

use crate::cli::{
    resolve_current_session_target, run_command, run_payload_command_resolved, ExitFailure,
};
use crate::cli_args::{
    build_scope, SetEnvironmentArgs, SetHookArgs, SetOptionArgs, SetOptionCommandKind,
    ShowEnvironmentArgs, ShowHooksArgs, ShowOptionsArgs, ShowOptionsCommandKind,
};
use options::{resolve_set_option_args, resolve_show_options_scope};

pub(crate) fn run_set_option(
    command: SetOptionCommandKind,
    args: SetOptionArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let request = resolve_set_option_args(command, args)?;

    run_command(socket_path, command.command_name(), move |connection| {
        connection.set_option_by_name(
            request.scope,
            request.option,
            request.value,
            request.mode,
            request.only_if_unset,
            request.unset,
            request.unset_pane_overrides,
        )
    })
}

pub(crate) fn run_set_environment(
    args: SetEnvironmentArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mode = resolve_set_environment_mode(&args)?;
    let value = match mode {
        Some(SetEnvironmentMode::Clear | SetEnvironmentMode::Unset) => String::new(),
        Some(SetEnvironmentMode::Set) | None => args
            .value
            .clone()
            .ok_or_else(|| ExitFailure::new(1, "set-environment requires a value"))?,
    };

    run_command(socket_path, "set-environment", move |connection| {
        connection.set_environment(
            build_scope(args.global, args.target),
            args.name,
            value,
            mode,
            args.hidden,
            args.format,
        )
    })
}

pub(crate) fn run_show_options(
    command: ShowOptionsCommandKind,
    args: ShowOptionsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let scope = resolve_show_options_scope(command, &args)?;
    let command_name = command.command_name();

    run_payload_command_resolved(socket_path, command_name, move |connection| {
        let scope = scope.resolve(connection, command_name)?;
        connection
            .show_options(scope, args.name, args.value_only)
            .map_err(ExitFailure::from_client)
    })
}

pub(crate) fn run_show_environment(
    args: ShowEnvironmentArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_payload_command_resolved(socket_path, "show-environment", move |connection| {
        let scope = resolve_show_environment_scope(connection, args.global, args.target)?;
        connection
            .show_environment(scope, args.name, args.hidden, args.shell_format)
            .map_err(ExitFailure::from_client)
    })
}

pub(crate) fn run_set_hook(args: SetHookArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let scope = resolve_hook_scope("set-hook", args.global, args.window, args.pane, args.target)?;
    rmux_core::validate_hook_registration(args.hook.hook, &scope)
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;

    run_command(socket_path, "set-hook", move |connection| {
        connection.set_hook_mutation(
            scope,
            args.hook.hook,
            args.command,
            HookLifecycle::Persistent,
            args.append,
            args.unset,
            args.run_immediately,
            args.hook.index,
        )
    })
}

pub(crate) fn run_show_hooks(args: ShowHooksArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let scope = resolve_show_hooks_scope(args.global, args.window, args.pane, args.target)?;
    let hook = args.hook;
    let window = args.window;
    let pane = args.pane;

    run_payload_command_resolved(socket_path, "show-hooks", move |connection| {
        let scope = match scope {
            ShowHooksScope::Resolved(scope) => scope,
            ShowHooksScope::CurrentSession => {
                ScopeSelector::Session(resolve_current_session_target(connection)?)
            }
        };
        if let Some(hook) = hook {
            rmux_core::validate_hook_scope(hook, &scope)
                .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        }
        connection
            .show_hooks(scope, window, pane, hook)
            .map_err(ExitFailure::from_client)
    })
}

fn resolve_show_environment_scope(
    connection: &mut rmux_client::Connection,
    global: bool,
    target: Option<rmux_proto::SessionName>,
) -> Result<ScopeSelector, ExitFailure> {
    if global {
        return Ok(ScopeSelector::Global);
    }
    target
        .map(ScopeSelector::Session)
        .map(Ok)
        .unwrap_or_else(|| resolve_current_session_target(connection).map(ScopeSelector::Session))
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

fn resolve_hook_scope(
    command: &str,
    global: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
) -> Result<ScopeSelector, ExitFailure> {
    if window && pane {
        return Err(ExitFailure::new(
            1,
            format!("{command} does not support combining -w and -p"),
        ));
    }

    if global {
        reject_target(command, target.as_ref(), "-g")?;
        return Ok(ScopeSelector::Global);
    }

    match (window, pane, target) {
        (true, false, Some(Target::Session(session_name))) => Ok(ScopeSelector::Window(
            rmux_proto::WindowTarget::new(session_name),
        )),
        (true, false, Some(Target::Window(target))) => Ok(ScopeSelector::Window(target)),
        (true, false, Some(Target::Pane(target))) => Ok(ScopeSelector::Window(
            rmux_proto::WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            ),
        )),
        (true, false, None) => Err(ExitFailure::new(
            1,
            format!("{command} -w requires a target"),
        )),
        (false, true, Some(Target::Pane(target))) => Ok(ScopeSelector::Pane(target)),
        (false, true, Some(_)) => Err(ExitFailure::new(
            1,
            format!("{command} -p requires a pane target"),
        )),
        (false, true, None) => Err(ExitFailure::new(
            1,
            format!("{command} -p requires a target"),
        )),
        (false, false, Some(Target::Session(session_name))) => {
            Ok(ScopeSelector::Session(session_name))
        }
        (false, false, Some(Target::Window(target))) => Ok(ScopeSelector::Window(target)),
        (false, false, Some(Target::Pane(target))) => Ok(ScopeSelector::Pane(target)),
        (false, false, None) => Err(ExitFailure::new(
            1,
            format!("{command} requires -g or a target"),
        )),
        (true, true, _) => unreachable!("validated conflicting hook scope flags"),
    }
}

fn resolve_show_hooks_scope(
    global: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
) -> Result<ShowHooksScope, ExitFailure> {
    if global {
        reject_target("show-hooks", target.as_ref(), "-g")?;
        return Ok(ShowHooksScope::Resolved(ScopeSelector::Global));
    }

    if !window && !pane && target.is_none() {
        return Ok(ShowHooksScope::CurrentSession);
    }

    resolve_hook_scope("show-hooks", false, window, pane, target).map(ShowHooksScope::Resolved)
}

enum ShowHooksScope {
    Resolved(ScopeSelector),
    CurrentSession,
}

fn reject_target(command: &str, target: Option<&Target>, flag: &str) -> Result<(), ExitFailure> {
    if target.is_some() {
        Err(ExitFailure::new(
            1,
            format!("{command} {flag} does not accept a target"),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{options::ShowOptionsScope, resolve_set_option_args, resolve_show_options_scope};
    use crate::cli_args::{
        SetOptionArgs, SetOptionCommandKind, ShowOptionsArgs, ShowOptionsCommandKind,
    };
    use rmux_proto::{OptionScopeSelector, PaneTarget, SessionName, Target, WindowTarget};

    fn global_set_args(option: &str, value: &str) -> SetOptionArgs {
        SetOptionArgs {
            global: true,
            server: false,
            window: false,
            pane: false,
            append: false,
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
                append: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(Target::Window(window.clone())),
                option: "pane-border-style".to_owned(),
                value: Some("fg=colour1".to_owned()),
            },
        )
        .expect("window-scoped set-window-option resolves");

        assert_eq!(resolved.scope, OptionScopeSelector::Window(window));
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
    fn set_option_server_flag_still_rejects_window_scoped_options() {
        let result = resolve_set_option_args(
            SetOptionCommandKind::SetOption,
            SetOptionArgs {
                server: true,
                ..global_set_args("mode-style", "fg=black,bg=red")
            },
        );
        let error = match result {
            Ok(_) => panic!("mode-style should not accept server scope"),
            Err(error) => error,
        };

        assert_eq!(
            error.message(),
            "server scope is not supported for this option"
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
    fn set_window_option_uses_current_window_for_session_targets() {
        let session = SessionName::new("alpha").expect("valid session");
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetWindowOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                append: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(Target::Session(session.clone())),
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
                append: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(Target::Session(session.clone())),
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
        let pane = PaneTarget::with_window(session.clone(), 0, 1);
        let resolved = resolve_set_option_args(
            SetOptionCommandKind::SetWindowOption,
            SetOptionArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                append: false,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                target: Some(Target::Pane(pane)),
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
    fn show_window_options_accepts_window_targets_without_server_scope() {
        let session = SessionName::new("alpha").expect("valid session");
        let window = WindowTarget::with_window(session, 0);
        let scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowWindowOptions,
            &ShowOptionsArgs {
                global: false,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                value_only: true,
                target: Some(Target::Window(window.clone())),
                name: Some("pane-border-style".to_owned()),
            },
        )
        .expect("window-target show-window-options resolves");

        assert_eq!(
            scope,
            ShowOptionsScope::Resolved(OptionScopeSelector::Window(window))
        );
    }

    #[test]
    fn show_window_options_uses_window_global_scope_with_g_flag() {
        let scope = resolve_show_options_scope(
            ShowOptionsCommandKind::ShowWindowOptions,
            &ShowOptionsArgs {
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
                global: true,
                server: true,
                window: false,
                pane: false,
                quiet: false,
                value_only: true,
                target: Some(Target::Session(
                    SessionName::new("missing").expect("valid session"),
                )),
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
                global: true,
                server: false,
                window: false,
                pane: false,
                quiet: false,
                value_only: true,
                target: Some(Target::Session(
                    SessionName::new("missing").expect("valid session"),
                )),
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
                append: false,
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
