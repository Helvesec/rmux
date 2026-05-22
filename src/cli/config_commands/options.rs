use rmux_client::Connection;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{SetOptionMode, Target, WindowTarget};

use crate::cli::ExitFailure;
use crate::cli_args::{
    SetOptionArgs, SetOptionCommandKind, ShowOptionsArgs, ShowOptionsCommandKind,
};

pub(super) fn resolve_set_option_args(
    command: SetOptionCommandKind,
    args: SetOptionArgs,
) -> Result<ResolvedSetOptionArgs, ExitFailure> {
    validate_set_option_name(&args.option)?;
    let scope = resolve_set_option_scope(
        command,
        &args.option,
        args.global,
        args.server,
        args.window,
        args.pane,
        args.target,
    )?;
    let mode = if args.append {
        SetOptionMode::Append
    } else {
        SetOptionMode::Replace
    };

    rmux_core::validate_option_name_mutation(
        &args.option,
        &scope,
        mode,
        args.value.as_deref(),
        args.unset,
    )
    .map_err(|error| ExitFailure::new(1, error.to_string()))?;

    Ok(ResolvedSetOptionArgs {
        scope,
        option: args.option,
        value: args.value,
        mode,
        only_if_unset: args.only_if_unset,
        unset: args.unset,
        unset_pane_overrides: args.unset_pane_overrides,
    })
}

pub(super) struct ResolvedSetOptionArgs {
    pub(super) scope: OptionScopeSelector,
    pub(super) option: String,
    pub(super) value: Option<String>,
    pub(super) mode: SetOptionMode,
    pub(super) only_if_unset: bool,
    pub(super) unset: bool,
    pub(super) unset_pane_overrides: bool,
}

fn validate_set_option_name(name: &str) -> Result<(), ExitFailure> {
    match rmux_core::resolve_option_name(name) {
        Ok(_) => Ok(()),
        Err(rmux_proto::RmuxError::Server(message)) if message.starts_with("unknown option: ") => {
            Err(ExitFailure::new(1, format!("invalid option: {name}")))
        }
        Err(error) => Err(ExitFailure::new(1, error.to_string())),
    }
}

fn resolve_set_option_scope(
    command: SetOptionCommandKind,
    option: &str,
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
) -> Result<OptionScopeSelector, ExitFailure> {
    let force_window = matches!(command, SetOptionCommandKind::SetWindowOption);
    let is_user = option
        .split('[')
        .next()
        .is_some_and(|base| base.starts_with('@'));
    let supports_scope = |scope: &OptionScopeSelector| {
        rmux_core::validate_option_name_mutation(option, scope, SetOptionMode::Replace, None, true)
            .is_ok()
    };

    if server {
        let scope = OptionScopeSelector::ServerGlobal;
        if !is_user && !supports_scope(&scope) {
            return Err(ExitFailure::new(
                1,
                "server scope is not supported for this option",
            ));
        }
        return Ok(scope);
    }

    if pane {
        let Some(Target::Pane(target)) = target else {
            return Err(ExitFailure::new(
                1,
                format!("{} -p requires a pane target", command.command_name()),
            ));
        };
        let scope = OptionScopeSelector::Pane(target);
        if !is_user && !supports_scope(&scope) {
            return Err(ExitFailure::new(
                1,
                "pane scope is not supported for this option",
            ));
        }
        return Ok(scope);
    }

    if window || force_window {
        if global {
            let scope = OptionScopeSelector::WindowGlobal;
            if !is_user && !supports_scope(&scope) {
                return Err(ExitFailure::new(
                    1,
                    "window scope is not supported for this option",
                ));
            }
            return Ok(scope);
        }

        let Some(target) = target else {
            let message = if force_window {
                "set-window-option requires a window target or -g"
            } else {
                "set-option requires a target or one of -g, -s, -w, or -p"
            };
            return Err(ExitFailure::new(1, message));
        };
        let scope = match target {
            Target::Session(session_name) => {
                OptionScopeSelector::Window(WindowTarget::new(session_name))
            }
            Target::Window(target) => OptionScopeSelector::Window(target),
            Target::Pane(target) => OptionScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )),
        };
        if !is_user && !supports_scope(&scope) {
            return Err(ExitFailure::new(
                1,
                "window scope is not supported for this option",
            ));
        }
        return Ok(scope);
    }

    if global {
        let scope = rmux_core::default_global_scope_for_option_name(option)
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        if !is_user && !supports_scope(&scope) {
            return Err(ExitFailure::new(
                1,
                "global scope is not supported for this option",
            ));
        }
        return Ok(scope);
    }

    let Some(target) = target else {
        return Err(ExitFailure::new(
            1,
            format!(
                "{} requires a target or one of -g, -s, -w, or -p",
                command.command_name()
            ),
        ));
    };

    let scope = match target {
        Target::Session(session_name) => {
            if is_user {
                OptionScopeSelector::Session(session_name)
            } else if supports_scope(&OptionScopeSelector::Window(WindowTarget::new(
                session_name.clone(),
            ))) {
                OptionScopeSelector::Window(WindowTarget::new(session_name))
            } else {
                OptionScopeSelector::Session(session_name)
            }
        }
        Target::Window(target) => {
            if is_user {
                OptionScopeSelector::Session(target.session_name().clone())
            } else if supports_scope(&OptionScopeSelector::Window(target.clone())) {
                OptionScopeSelector::Window(target)
            } else {
                OptionScopeSelector::Session(target.session_name().clone())
            }
        }
        Target::Pane(target) => {
            if is_user {
                OptionScopeSelector::Session(target.session_name().clone())
            } else if supports_scope(&OptionScopeSelector::Pane(target.clone())) {
                OptionScopeSelector::Pane(target)
            } else if supports_scope(&OptionScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            ))) {
                OptionScopeSelector::Window(WindowTarget::with_window(
                    target.session_name().clone(),
                    target.window_index(),
                ))
            } else {
                OptionScopeSelector::Session(target.session_name().clone())
            }
        }
    };

    if !is_user && !supports_scope(&scope) {
        return Err(ExitFailure::new(
            1,
            "target scope is not supported for this option",
        ));
    }

    Ok(scope)
}

pub(super) fn resolve_show_options_scope(
    command: ShowOptionsCommandKind,
    args: &ShowOptionsArgs,
) -> Result<ShowOptionsScope, ExitFailure> {
    let force_window = matches!(command, ShowOptionsCommandKind::ShowWindowOptions);
    let command_name = command.command_name();
    if args.server {
        return Ok(OptionScopeSelector::ServerGlobal.into());
    }

    match (args.window || force_window, args.pane, args.target.as_ref()) {
        (true, false, _) if args.global => Ok(OptionScopeSelector::WindowGlobal.into()),
        (true, false, Some(Target::Session(session_name))) => Ok(OptionScopeSelector::Window(
            rmux_proto::WindowTarget::new(session_name.clone()),
        )
        .into()),
        (true, false, Some(Target::Window(target))) => {
            Ok(OptionScopeSelector::Window(target.clone()).into())
        }
        (true, false, Some(Target::Pane(target))) => Ok(OptionScopeSelector::Window(
            rmux_proto::WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            ),
        )
        .into()),
        (true, false, None) if force_window => Ok(ShowOptionsScope::CurrentWindow),
        (true, false, None) => Err(ExitFailure::new(
            1,
            format!("{command_name} -w requires a target"),
        )),
        (false, true, _) if args.global => Err(ExitFailure::new(
            1,
            format!("{command_name} does not support combining -g and -p"),
        )),
        (false, true, Some(Target::Pane(target))) => {
            Ok(OptionScopeSelector::Pane(target.clone()).into())
        }
        (false, true, Some(_)) => Err(ExitFailure::new(
            1,
            format!("{command_name} -p requires a pane target"),
        )),
        (false, true, None) => Err(ExitFailure::new(
            1,
            format!("{command_name} -p requires a target"),
        )),
        (false, false, _) if args.global => Ok(if let Some(name) = args.name.as_deref() {
            rmux_core::default_global_scope_for_option_name(name)
                .map_err(|error| ExitFailure::new(1, error.to_string()))?
        } else if force_window {
            OptionScopeSelector::WindowGlobal
        } else {
            OptionScopeSelector::SessionGlobal
        }
        .into()),
        (false, false, Some(Target::Session(session_name))) => {
            Ok(OptionScopeSelector::Session(session_name.clone()).into())
        }
        (false, false, Some(Target::Window(target))) => {
            Ok(OptionScopeSelector::Window(target.clone()).into())
        }
        (false, false, Some(Target::Pane(target))) => {
            Ok(OptionScopeSelector::Pane(target.clone()).into())
        }
        (false, false, None) if force_window => Ok(ShowOptionsScope::CurrentWindow),
        (false, false, None) => Ok(ShowOptionsScope::CurrentSession),
        (true, true, _) => unreachable!("clap scope group prevents -w and -p together"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShowOptionsScope {
    Resolved(OptionScopeSelector),
    CurrentSession,
    CurrentWindow,
}

impl ShowOptionsScope {
    pub(super) fn resolve(
        self,
        connection: &mut Connection,
        command_name: &str,
    ) -> Result<OptionScopeSelector, ExitFailure> {
        match self {
            Self::Resolved(scope) => Ok(scope),
            Self::CurrentSession => super::super::resolve_current_session_target(connection)
                .map(OptionScopeSelector::Session),
            Self::CurrentWindow => {
                super::super::resolve_window_target_or_current(connection, None, command_name)
                    .map(OptionScopeSelector::Window)
            }
        }
    }
}

impl From<OptionScopeSelector> for ShowOptionsScope {
    fn from(scope: OptionScopeSelector) -> Self {
        Self::Resolved(scope)
    }
}
