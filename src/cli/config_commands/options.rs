use rmux_client::Connection;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{PaneTarget, ResolveTargetType, SetOptionMode, Target, WindowTarget};

use crate::cli::ExitFailure;
use crate::cli_args::{
    SetOptionArgs, SetOptionCommandKind, ShowOptionsArgs, ShowOptionsCommandKind, TargetSpec,
};

use super::super::{
    resolve_current_pane_target, resolve_current_session_target, resolve_target_spec,
    resolve_window_target_or_current,
};

pub(super) fn resolve_set_option_args(
    connection: &mut Connection,
    command: SetOptionCommandKind,
    args: SetOptionArgs,
) -> Result<ResolvedSetOptionCommand, ExitFailure> {
    validate_set_option_name(&args.option)?;
    let request = SetOptionScopeRequest::new(command, &args);
    let scope = resolve_set_option_scope(
        request,
        &mut ConnectionSetOptionTargetResolver { connection },
    )?;
    build_resolved_set_option_command(args, scope)
}

#[cfg(test)]
pub(super) fn resolve_set_option_args_with_exact_targets(
    command: SetOptionCommandKind,
    args: SetOptionArgs,
) -> Result<ResolvedSetOptionCommand, ExitFailure> {
    validate_set_option_name(&args.option)?;
    let mut resolver = ExactSetOptionTargetResolver;
    let request = SetOptionScopeRequest::new(command, &args);
    let scope = resolve_set_option_scope(request, &mut resolver)?;
    build_resolved_set_option_command(args, scope)
}

fn build_resolved_set_option_command(
    args: SetOptionArgs,
    scope: ResolvedSetOptionScope,
) -> Result<ResolvedSetOptionCommand, ExitFailure> {
    let Some(scope) = scope.into_scope() else {
        return Ok(ResolvedSetOptionCommand::NoOp);
    };

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

    Ok(ResolvedSetOptionCommand::Request(ResolvedSetOptionArgs {
        scope,
        option: args.option,
        value: args.value,
        mode,
        only_if_unset: args.only_if_unset,
        unset: args.unset,
        unset_pane_overrides: args.unset_pane_overrides,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedSetOptionCommand {
    Request(ResolvedSetOptionArgs),
    NoOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedSetOptionArgs {
    pub(super) scope: OptionScopeSelector,
    pub(super) option: String,
    pub(super) value: Option<String>,
    pub(super) mode: SetOptionMode,
    pub(super) only_if_unset: bool,
    pub(super) unset: bool,
    pub(super) unset_pane_overrides: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedSetOptionScope {
    Scope(OptionScopeSelector),
    NoOp,
}

impl ResolvedSetOptionScope {
    fn into_scope(self) -> Option<OptionScopeSelector> {
        match self {
            Self::Scope(scope) => Some(scope),
            Self::NoOp => None,
        }
    }
}

impl From<OptionScopeSelector> for ResolvedSetOptionScope {
    fn from(scope: OptionScopeSelector) -> Self {
        Self::Scope(scope)
    }
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

struct SetOptionScopeRequest<'a> {
    command: SetOptionCommandKind,
    option: &'a str,
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    target: Option<&'a TargetSpec>,
}

impl<'a> SetOptionScopeRequest<'a> {
    fn new(command: SetOptionCommandKind, args: &'a SetOptionArgs) -> Self {
        Self {
            command,
            option: &args.option,
            global: args.global,
            server: args.server,
            window: args.window,
            pane: args.pane,
            target: args.target.as_ref(),
        }
    }
}

fn resolve_set_option_scope(
    request: SetOptionScopeRequest<'_>,
    resolver: &mut impl SetOptionTargetResolver,
) -> Result<ResolvedSetOptionScope, ExitFailure> {
    let force_window = matches!(request.command, SetOptionCommandKind::SetWindowOption);
    let is_user = request
        .option
        .split('[')
        .next()
        .is_some_and(|base| base.starts_with('@'));
    let supports_scope = |scope: &OptionScopeSelector| {
        rmux_core::validate_option_name_mutation(
            request.option,
            scope,
            SetOptionMode::Replace,
            None,
            true,
        )
        .is_ok()
    };

    if request.server {
        let scope = OptionScopeSelector::ServerGlobal;
        if is_user || supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Ok(ResolvedSetOptionScope::NoOp);
    }

    if request.pane {
        let target = match request.target {
            Some(target) => resolver.resolve_target(target, ResolveTargetType::Pane)?,
            None => Target::Pane(resolver.current_pane(request.command.command_name())?),
        };
        let Target::Pane(target) = target else {
            return Err(ExitFailure::new(
                1,
                format!(
                    "{} -p requires a pane target",
                    request.command.command_name()
                ),
            ));
        };
        let scope = OptionScopeSelector::Pane(target);
        if !is_user && !supports_scope(&scope) {
            return Err(ExitFailure::new(
                1,
                "pane scope is not supported for this option",
            ));
        }
        return Ok(scope.into());
    }

    if request.window || force_window {
        if request.global {
            let scope = OptionScopeSelector::WindowGlobal;
            if !is_user && !supports_scope(&scope) {
                return Err(ExitFailure::new(
                    1,
                    "window scope is not supported for this option",
                ));
            }
            return Ok(scope.into());
        }

        let target = match request.target {
            Some(target) => resolver.resolve_target(target, ResolveTargetType::Window)?,
            None => Target::Window(resolver.current_window(request.command.command_name())?),
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
        return Ok(scope.into());
    }

    if request.global {
        let scope = rmux_core::default_global_scope_for_option_name(request.option)
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        if !is_user && !supports_scope(&scope) {
            return Err(ExitFailure::new(
                1,
                "global scope is not supported for this option",
            ));
        }
        return Ok(scope.into());
    }

    let Some(target_spec) = request.target else {
        return resolve_implicit_set_option_scope(request.option, resolver);
    };

    let target = resolver.resolve_target(target_spec, ResolveTargetType::Session)?;

    if !is_user {
        let global_scope = rmux_core::default_global_scope_for_option_name(request.option)
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        if matches!(global_scope, OptionScopeSelector::ServerGlobal)
            && supports_scope(&global_scope)
        {
            return Ok(global_scope.into());
        }
    }

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

    Ok(scope.into())
}

fn resolve_implicit_set_option_scope(
    option: &str,
    resolver: &mut impl SetOptionTargetResolver,
) -> Result<ResolvedSetOptionScope, ExitFailure> {
    match rmux_core::default_global_scope_for_option_name(option)
        .map_err(|error| ExitFailure::new(1, error.to_string()))?
    {
        OptionScopeSelector::ServerGlobal => Ok(OptionScopeSelector::ServerGlobal.into()),
        OptionScopeSelector::WindowGlobal => {
            Ok(OptionScopeSelector::Window(resolver.current_window("set-option")?).into())
        }
        OptionScopeSelector::SessionGlobal => {
            Ok(OptionScopeSelector::Session(resolver.current_session("set-option")?).into())
        }
        scope => Ok(scope.into()),
    }
}

trait SetOptionTargetResolver {
    fn resolve_target(
        &mut self,
        target: &TargetSpec,
        target_type: ResolveTargetType,
    ) -> Result<Target, ExitFailure>;

    fn current_session(
        &mut self,
        command_name: &str,
    ) -> Result<rmux_proto::SessionName, ExitFailure>;

    fn current_pane(&mut self, command_name: &str) -> Result<PaneTarget, ExitFailure>;

    fn current_window(&mut self, command_name: &str) -> Result<WindowTarget, ExitFailure>;
}

struct ConnectionSetOptionTargetResolver<'a> {
    connection: &'a mut Connection,
}

impl SetOptionTargetResolver for ConnectionSetOptionTargetResolver<'_> {
    fn resolve_target(
        &mut self,
        target: &TargetSpec,
        target_type: ResolveTargetType,
    ) -> Result<Target, ExitFailure> {
        resolve_target_spec(self.connection, target, target_type, false, false)
    }

    fn current_session(
        &mut self,
        _command_name: &str,
    ) -> Result<rmux_proto::SessionName, ExitFailure> {
        resolve_current_session_target(self.connection)
    }

    fn current_pane(&mut self, command_name: &str) -> Result<PaneTarget, ExitFailure> {
        resolve_current_pane_target(self.connection, command_name)
    }

    fn current_window(&mut self, command_name: &str) -> Result<WindowTarget, ExitFailure> {
        resolve_window_target_or_current(self.connection, None, command_name)
    }
}

#[cfg(test)]
struct ExactSetOptionTargetResolver;

#[cfg(test)]
impl SetOptionTargetResolver for ExactSetOptionTargetResolver {
    fn resolve_target(
        &mut self,
        target: &TargetSpec,
        _target_type: ResolveTargetType,
    ) -> Result<Target, ExitFailure> {
        target
            .exact()
            .cloned()
            .ok_or_else(|| ExitFailure::new(1, "test target requires daemon resolution"))
    }

    fn current_session(
        &mut self,
        _command_name: &str,
    ) -> Result<rmux_proto::SessionName, ExitFailure> {
        Err(ExitFailure::new(
            1,
            "test path does not provide a current session",
        ))
    }

    fn current_pane(&mut self, _command_name: &str) -> Result<PaneTarget, ExitFailure> {
        Err(ExitFailure::new(
            1,
            "test path does not provide a current pane",
        ))
    }

    fn current_window(&mut self, _command_name: &str) -> Result<WindowTarget, ExitFailure> {
        Err(ExitFailure::new(
            1,
            "test path does not provide a current window",
        ))
    }
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
        (true, false, Some(target)) => Ok(ShowOptionsScope::Unresolved {
            target: target.clone(),
            kind: UnresolvedShowOptionsScope::Window,
        }),
        (true, false, None) if force_window => Ok(ShowOptionsScope::CurrentWindow),
        (true, false, None) => Err(ExitFailure::new(
            1,
            format!("{command_name} -w requires a target"),
        )),
        (false, true, _) if args.global => Err(ExitFailure::new(
            1,
            format!("{command_name} does not support combining -g and -p"),
        )),
        (false, true, Some(target)) => Ok(ShowOptionsScope::Unresolved {
            target: target.clone(),
            kind: UnresolvedShowOptionsScope::Pane,
        }),
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
        (false, false, Some(target)) => Ok(ShowOptionsScope::Unresolved {
            target: target.clone(),
            kind: UnresolvedShowOptionsScope::Natural,
        }),
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
    Unresolved {
        target: TargetSpec,
        kind: UnresolvedShowOptionsScope,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnresolvedShowOptionsScope {
    Window,
    Pane,
    Natural,
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
        UnresolvedShowOptionsScope::Window => ResolveTargetType::Window,
        UnresolvedShowOptionsScope::Pane => ResolveTargetType::Pane,
        UnresolvedShowOptionsScope::Natural => natural_target_type(target.raw()),
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
        (UnresolvedShowOptionsScope::Natural, Target::Session(session_name)) => {
            Ok(OptionScopeSelector::Session(session_name))
        }
        (UnresolvedShowOptionsScope::Natural, Target::Window(target)) => {
            Ok(OptionScopeSelector::Window(target))
        }
        (UnresolvedShowOptionsScope::Natural, Target::Pane(target)) => {
            Ok(OptionScopeSelector::Pane(target))
        }
    }
}

fn natural_target_type(raw: &str) -> ResolveTargetType {
    if raw.starts_with('%') || raw.rsplit_once('.').is_some() {
        ResolveTargetType::Pane
    } else if raw.starts_with('@')
        || raw
            .rsplit_once(':')
            .is_some_and(|(_, rest)| !rest.is_empty())
    {
        ResolveTargetType::Window
    } else {
        ResolveTargetType::Session
    }
}
