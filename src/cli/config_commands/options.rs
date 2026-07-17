use rmux_client::Connection;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{PaneTarget, ResolveTargetType, SessionName, SetOptionMode, Target, WindowTarget};

#[path = "options/show_scope.rs"]
mod show_scope;

use crate::cli::ExitFailure;
use crate::cli_args::{SetOptionArgs, SetOptionCommandKind, TargetSpec};
use crate::cli_response::tmux_cli_error_message;

use super::super::{
    resolve_current_pane_target, resolve_current_session_target, resolve_target_spec,
    resolve_window_target_or_current,
};
pub(super) use show_scope::resolve_show_options_scope;
#[cfg(test)]
pub(super) use show_scope::{ShowOptionsScope, UnresolvedShowOptionsScope};

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
    let format_target = if args.format {
        Some(resolve_set_option_format_target(
            connection,
            command.command_name(),
            args.target.as_ref(),
        )?)
    } else {
        None
    };
    build_resolved_set_option_command(command, args, scope, format_target)
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
    build_resolved_set_option_command(command, args, scope, None)
}

fn resolve_set_option_format_target(
    connection: &mut Connection,
    command_name: &str,
    target: Option<&TargetSpec>,
) -> Result<Target, ExitFailure> {
    match target {
        Some(target) => {
            resolve_target_spec(connection, target, ResolveTargetType::Pane, false, false)
        }
        None => resolve_current_pane_target(connection, command_name).map(Target::Pane),
    }
}

fn build_resolved_set_option_command(
    command: SetOptionCommandKind,
    args: SetOptionArgs,
    scope: ResolvedSetOptionScope,
    format_target: Option<Target>,
) -> Result<ResolvedSetOptionCommand, ExitFailure> {
    let Some(scope) = scope.into_scope() else {
        return Ok(ResolvedSetOptionCommand::NoOp);
    };

    let mode = if args.append {
        SetOptionMode::Append
    } else {
        SetOptionMode::Replace
    };
    let unset = args.unset || args.unset_pane_overrides;

    if !args.format {
        rmux_core::validate_option_name_mutation(
            &args.option,
            &scope,
            mode,
            args.value.as_deref(),
            unset,
        )
        .map_err(|error| {
            ExitFailure::new(1, tmux_cli_error_message(command.command_name(), &error))
        })?;
    }

    Ok(ResolvedSetOptionCommand::Request(ResolvedSetOptionArgs {
        scope,
        option: args.option,
        value: args.value,
        mode,
        only_if_unset: args.only_if_unset,
        unset,
        unset_pane_overrides: args.unset_pane_overrides,
        format: args.format,
        format_target,
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
    pub(super) format: bool,
    pub(super) format_target: Option<Target>,
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
        Err(rmux_proto::RmuxError::Server(message))
            if message.starts_with("unknown option: ")
                || message.starts_with("invalid option: ") =>
        {
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
    let supports_scope =
        |scope: &OptionScopeSelector| option_name_supports_scope(request.option, scope);
    let window = request.window || force_window;

    if request.pane
        && !request.server
        && !window
        && (is_user || option_supports_pane_scope(request.option))
    {
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
        return Ok(OptionScopeSelector::Pane(target).into());
    }

    if request.global && !is_user && (request.server || request.pane || window) {
        let scope = rmux_core::default_global_scope_for_option_name(request.option)
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        if supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Err(ExitFailure::new(
            1,
            "global scope is not supported for this option",
        ));
    }

    if !request.global && !is_user && (request.server || request.pane || window) {
        let scope = resolve_natural_known_set_option_scope(
            request.option,
            request.target,
            request.command.command_name(),
            resolver,
        )?;
        return Ok(scope.into());
    }

    if request.global
        && !request.server
        && !request.window
        && !request.pane
        && !force_window
        && !is_user
    {
        let scope = rmux_core::default_global_scope_for_option_name(request.option)
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        if supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Err(ExitFailure::new(
            1,
            "global scope is not supported for this option",
        ));
    }

    if request.server {
        let scope = OptionScopeSelector::ServerGlobal;
        if is_user || supports_scope(&scope) {
            return Ok(scope.into());
        }
        if !request.global {
            return Ok(ResolvedSetOptionScope::NoOp);
        }
    }

    if request.global && request.pane && !force_window {
        return Ok(OptionScopeSelector::WindowGlobal.into());
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
        return Ok(scope.into());
    }

    if window {
        if request.global {
            let scope = OptionScopeSelector::WindowGlobal;
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

    if !is_user {
        let global_scope = rmux_core::default_global_scope_for_option_name(request.option)
            .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        if matches!(global_scope, OptionScopeSelector::ServerGlobal)
            && supports_scope(&global_scope)
        {
            return Ok(global_scope.into());
        }

        let target = resolver.resolve_target(target_spec, target_type_for_scope(&global_scope))?;
        let scope = match target {
            Target::Session(session_name) => {
                if supports_scope(&OptionScopeSelector::Window(WindowTarget::new(
                    session_name.clone(),
                ))) {
                    OptionScopeSelector::Window(WindowTarget::new(session_name))
                } else {
                    OptionScopeSelector::Session(session_name)
                }
            }
            Target::Window(target) => {
                if supports_scope(&OptionScopeSelector::Window(target.clone())) {
                    OptionScopeSelector::Window(target)
                } else {
                    OptionScopeSelector::Session(target.session_name().clone())
                }
            }
            Target::Pane(target) => {
                if supports_scope(&OptionScopeSelector::Pane(target.clone())) {
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

        if !supports_scope(&scope) {
            return Err(ExitFailure::new(
                1,
                "target scope is not supported for this option",
            ));
        }
        return Ok(scope.into());
    }

    let target = resolver.resolve_target(target_spec, ResolveTargetType::Session)?;
    let scope = match target {
        Target::Session(session_name) => OptionScopeSelector::Session(session_name),
        Target::Window(target) => OptionScopeSelector::Session(target.session_name().clone()),
        Target::Pane(target) => OptionScopeSelector::Session(target.session_name().clone()),
    };

    if !is_user && !supports_scope(&scope) {
        return Err(ExitFailure::new(
            1,
            "target scope is not supported for this option",
        ));
    }

    Ok(scope.into())
}

fn resolve_natural_known_set_option_scope(
    option: &str,
    target: Option<&TargetSpec>,
    command_name: &str,
    resolver: &mut impl SetOptionTargetResolver,
) -> Result<OptionScopeSelector, ExitFailure> {
    let scope = rmux_core::default_global_scope_for_option_name(option)
        .map_err(|error| ExitFailure::new(1, error.to_string()))?;
    match scope {
        OptionScopeSelector::ServerGlobal => Ok(OptionScopeSelector::ServerGlobal),
        OptionScopeSelector::SessionGlobal => {
            let session_name = match target {
                Some(target) => {
                    match resolver.resolve_target(target, ResolveTargetType::Session)? {
                        Target::Session(session_name) => session_name,
                        Target::Window(target) => target.session_name().clone(),
                        Target::Pane(target) => target.session_name().clone(),
                    }
                }
                None => resolver.current_session(command_name)?,
            };
            Ok(OptionScopeSelector::Session(session_name))
        }
        OptionScopeSelector::WindowGlobal => {
            let window = match target {
                Some(target) => match resolver.resolve_target(target, ResolveTargetType::Window)? {
                    Target::Session(session_name) => WindowTarget::new(session_name),
                    Target::Window(target) => target,
                    Target::Pane(target) => WindowTarget::with_window(
                        target.session_name().clone(),
                        target.window_index(),
                    ),
                },
                None => resolver.current_window(command_name)?,
            };
            Ok(OptionScopeSelector::Window(window))
        }
        OptionScopeSelector::Session(session_name) => {
            Ok(OptionScopeSelector::Session(session_name))
        }
        OptionScopeSelector::Window(target) => Ok(OptionScopeSelector::Window(target)),
        OptionScopeSelector::Pane(target) => Ok(OptionScopeSelector::Pane(target)),
    }
}

fn option_supports_pane_scope(option: &str) -> bool {
    option_name_supports_scope(option, &dummy_pane_scope())
}

fn dummy_pane_scope() -> OptionScopeSelector {
    OptionScopeSelector::Pane(PaneTarget::with_window(
        SessionName::new("set-option").expect("valid session name"),
        0,
        0,
    ))
}

fn option_name_supports_scope(option: &str, scope: &OptionScopeSelector) -> bool {
    rmux_core::resolve_option_name(option)
        .map(|query| query.supports_scope(scope))
        .unwrap_or(false)
}

fn target_type_for_scope(scope: &OptionScopeSelector) -> ResolveTargetType {
    match scope {
        OptionScopeSelector::WindowGlobal | OptionScopeSelector::Window(_) => {
            ResolveTargetType::Window
        }
        OptionScopeSelector::Pane(_) => ResolveTargetType::Pane,
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::Session(_) => ResolveTargetType::Session,
    }
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
