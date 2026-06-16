use rmux_client::Connection;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{ResolveTargetType, RmuxError, Target, WindowTarget};

use crate::cli::ExitFailure;
use crate::cli_args::{ShowOptionsArgs, ShowOptionsCommandKind, TargetSpec};

use super::super::super::{
    resolve_current_pane_target, resolve_current_session_target, resolve_target_spec,
    resolve_window_target_or_current,
};

pub(in crate::cli::config_commands) fn resolve_show_options_scope(
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
        (true, false, Some(target)) => Ok(ShowOptionsScope::Unresolved {
            target: target.clone(),
            kind: UnresolvedShowOptionsScope::Window,
        }),
        (true, false, None) => Ok(ShowOptionsScope::CurrentWindow),
        (false, true, _) if args.global => Err(ExitFailure::new(
            1,
            format!("{command_name} does not support combining -g and -p"),
        )),
        (false, true, Some(target)) => Ok(ShowOptionsScope::Unresolved {
            target: target.clone(),
            kind: UnresolvedShowOptionsScope::Pane,
        }),
        (false, true, None) => Ok(ShowOptionsScope::CurrentPane),
        (false, false, _) if args.global => Ok(if let Some(name) = args.name.as_deref() {
            rmux_core::default_global_scope_for_option_name(name)
                .map_err(option_lookup_exit_failure)?
        } else if force_window {
            OptionScopeSelector::WindowGlobal
        } else {
            OptionScopeSelector::SessionGlobal
        }
        .into()),
        (false, false, Some(target)) => show_options_scope_for_target(target, args.name.as_deref()),
        (false, false, None) if force_window => Ok(ShowOptionsScope::CurrentWindow),
        (false, false, None) => Ok(ShowOptionsScope::CurrentSession),
        (true, true, _) => unreachable!("clap scope group prevents -w and -p together"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli::config_commands) enum ShowOptionsScope {
    Resolved(OptionScopeSelector),
    CurrentSession,
    CurrentWindow,
    CurrentPane,
    Unresolved {
        target: TargetSpec,
        kind: UnresolvedShowOptionsScope,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli::config_commands) enum UnresolvedShowOptionsScope {
    Session,
    Window,
    Pane,
}

impl ShowOptionsScope {
    pub(in crate::cli::config_commands) fn resolve(
        self,
        connection: &mut Connection,
        command_name: &str,
    ) -> Result<OptionScopeSelector, ExitFailure> {
        match self {
            Self::Resolved(scope) => Ok(scope),
            Self::CurrentSession => {
                resolve_current_session_target(connection).map(OptionScopeSelector::Session)
            }
            Self::CurrentWindow => resolve_window_target_or_current(connection, None, command_name)
                .map(OptionScopeSelector::Window),
            Self::CurrentPane => {
                resolve_current_pane_target(connection, command_name).map(OptionScopeSelector::Pane)
            }
            Self::Unresolved { target, kind } => {
                resolve_unresolved_show_options_scope(connection, &target, kind)
            }
        }
    }
}

impl From<OptionScopeSelector> for ShowOptionsScope {
    fn from(scope: OptionScopeSelector) -> Self {
        Self::Resolved(scope)
    }
}

fn resolve_unresolved_show_options_scope(
    connection: &mut Connection,
    target: &TargetSpec,
    kind: UnresolvedShowOptionsScope,
) -> Result<OptionScopeSelector, ExitFailure> {
    let target_type = match kind {
        UnresolvedShowOptionsScope::Session => ResolveTargetType::Session,
        UnresolvedShowOptionsScope::Window => ResolveTargetType::Window,
        UnresolvedShowOptionsScope::Pane => ResolveTargetType::Pane,
    };
    let target = resolve_target_spec(connection, target, target_type, false, false)?;
    match (kind, target) {
        (UnresolvedShowOptionsScope::Pane, Target::Pane(target)) => {
            Ok(OptionScopeSelector::Pane(target))
        }
        (UnresolvedShowOptionsScope::Pane, _) => Err(ExitFailure::new(
            1,
            "show-options -p requires a pane target",
        )),
        (UnresolvedShowOptionsScope::Session, Target::Session(session_name)) => {
            Ok(OptionScopeSelector::Session(session_name))
        }
        (UnresolvedShowOptionsScope::Session, Target::Window(target)) => {
            Ok(OptionScopeSelector::Session(target.session_name().clone()))
        }
        (UnresolvedShowOptionsScope::Session, Target::Pane(target)) => {
            Ok(OptionScopeSelector::Session(target.session_name().clone()))
        }
        (UnresolvedShowOptionsScope::Window, Target::Session(session_name)) => {
            Ok(OptionScopeSelector::Window(WindowTarget::new(session_name)))
        }
        (UnresolvedShowOptionsScope::Window, Target::Window(target)) => {
            Ok(OptionScopeSelector::Window(target))
        }
        (UnresolvedShowOptionsScope::Window, Target::Pane(target)) => {
            Ok(OptionScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )))
        }
    }
}

fn show_options_scope_for_target(
    target: &TargetSpec,
    name: Option<&str>,
) -> Result<ShowOptionsScope, ExitFailure> {
    let Some(name) = name else {
        return Ok(ShowOptionsScope::Unresolved {
            target: target.clone(),
            kind: UnresolvedShowOptionsScope::Session,
        });
    };

    match rmux_core::default_global_scope_for_option_name(name)
        .map_err(option_lookup_exit_failure)?
    {
        OptionScopeSelector::ServerGlobal => Ok(OptionScopeSelector::ServerGlobal.into()),
        OptionScopeSelector::WindowGlobal | OptionScopeSelector::Window(_) => {
            Ok(ShowOptionsScope::Unresolved {
                target: target.clone(),
                kind: UnresolvedShowOptionsScope::Window,
            })
        }
        OptionScopeSelector::Pane(_) => Ok(ShowOptionsScope::Unresolved {
            target: target.clone(),
            kind: UnresolvedShowOptionsScope::Pane,
        }),
        OptionScopeSelector::SessionGlobal | OptionScopeSelector::Session(_) => {
            Ok(ShowOptionsScope::Unresolved {
                target: target.clone(),
                kind: UnresolvedShowOptionsScope::Session,
            })
        }
    }
}

fn option_lookup_exit_failure(error: RmuxError) -> ExitFailure {
    match error {
        RmuxError::Server(message) | RmuxError::Message(message) => {
            let normalized = message.strip_prefix("server error: ").unwrap_or(&message);
            ExitFailure::new(1, normalized.to_owned())
        }
        error => ExitFailure::new(1, error.to_string()),
    }
}
