use std::path::Path;

use rmux_client::Connection;
use rmux_proto::{HookLifecycle, HookName, ResolveTargetType, ScopeSelector, Target, WindowTarget};

use crate::cli::{
    resolve_current_pane_target, resolve_current_session_target, resolve_target_spec,
    run_command_resolved, run_payload_command_resolved, ExitFailure,
};
use crate::cli_args::{SetHookArgs, ShowHooksArgs, TargetSpec};

pub(crate) fn run_set_hook(args: SetHookArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let SetHookArgs {
        append,
        global,
        pane,
        run_immediately,
        target,
        unset,
        window,
        hook,
        command,
    } = args;
    let scope = resolve_hook_scope("set-hook", global, window, pane, target)?;

    run_command_resolved(socket_path, "set-hook", move |connection| {
        let scope = scope.resolve(connection, "set-hook")?;
        validate_hook_registration(hook.hook, &scope)?;
        connection
            .set_hook_mutation(
                scope,
                hook.hook,
                command,
                HookLifecycle::Persistent,
                append,
                unset,
                run_immediately,
                hook.index,
            )
            .map_err(ExitFailure::from_client)
    })
}

pub(crate) fn run_show_hooks(args: ShowHooksArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let scope = resolve_show_hooks_scope(args.global, args.window, args.pane, args.target)?;
    let hook = args.hook;
    let window = args.window;
    let pane = args.pane;

    run_payload_command_resolved(socket_path, "show-hooks", move |connection| {
        let scope = scope.resolve(connection, "show-hooks")?;
        if let Some(hook) = hook {
            rmux_core::validate_hook_scope(hook, &scope)
                .map_err(|error| ExitFailure::new(1, error.to_string()))?;
        }
        connection
            .show_hooks(scope, window, pane, hook)
            .map_err(ExitFailure::from_client)
    })
}

fn resolve_hook_scope(
    command: &str,
    global: bool,
    window: bool,
    pane: bool,
    target: Option<TargetSpec>,
) -> Result<HookScope, ExitFailure> {
    if window && pane {
        return Err(ExitFailure::new(
            1,
            format!("{command} does not support combining -w and -p"),
        ));
    }

    if global {
        reject_target(command, target.as_ref(), "-g")?;
        return Ok(HookScope::Resolved(ScopeSelector::Global));
    }

    match (window, pane, target) {
        (true, false, Some(target)) => Ok(HookScope::Unresolved {
            target,
            kind: HookTargetKind::Window,
        }),
        (true, false, None) => Ok(HookScope::CurrentWindow),
        (false, true, Some(target)) => Ok(HookScope::Unresolved {
            target,
            kind: HookTargetKind::Pane,
        }),
        (false, true, None) => Ok(HookScope::CurrentPane),
        (false, false, Some(target)) => Ok(HookScope::Unresolved {
            target,
            kind: HookTargetKind::Natural,
        }),
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
    target: Option<TargetSpec>,
) -> Result<ShowHooksScope, ExitFailure> {
    if global {
        reject_target("show-hooks", target.as_ref(), "-g")?;
        return Ok(ShowHooksScope(HookScope::Resolved(ScopeSelector::Global)));
    }

    if !window && !pane && target.is_none() {
        return Ok(ShowHooksScope(HookScope::CurrentSession));
    }

    resolve_hook_scope("show-hooks", false, window, pane, target).map(ShowHooksScope)
}

#[derive(Debug, Clone)]
struct ShowHooksScope(HookScope);

#[derive(Debug, Clone)]
enum HookScope {
    Resolved(ScopeSelector),
    CurrentSession,
    CurrentWindow,
    CurrentPane,
    Unresolved {
        target: TargetSpec,
        kind: HookTargetKind,
    },
}

#[derive(Debug, Clone, Copy)]
enum HookTargetKind {
    Window,
    Pane,
    Natural,
}

impl ShowHooksScope {
    fn resolve(
        self,
        connection: &mut Connection,
        command: &str,
    ) -> Result<ScopeSelector, ExitFailure> {
        self.0.resolve(connection, command)
    }
}

impl HookScope {
    fn resolve(
        self,
        connection: &mut Connection,
        command: &str,
    ) -> Result<ScopeSelector, ExitFailure> {
        match self {
            Self::Resolved(scope) => Ok(scope),
            Self::CurrentSession => {
                resolve_current_session_target(connection).map(ScopeSelector::Session)
            }
            Self::CurrentWindow => {
                let pane = resolve_current_pane_target(connection, command)?;
                Ok(ScopeSelector::Window(WindowTarget::with_window(
                    pane.session_name().clone(),
                    pane.window_index(),
                )))
            }
            Self::CurrentPane => {
                resolve_current_pane_target(connection, command).map(ScopeSelector::Pane)
            }
            Self::Unresolved { target, kind } => {
                resolve_unresolved_hook_scope(connection, command, &target, kind)
            }
        }
    }
}

fn resolve_unresolved_hook_scope(
    connection: &mut Connection,
    command: &str,
    target: &TargetSpec,
    kind: HookTargetKind,
) -> Result<ScopeSelector, ExitFailure> {
    let target_type = match kind {
        HookTargetKind::Window => ResolveTargetType::Window,
        HookTargetKind::Pane => ResolveTargetType::Pane,
        HookTargetKind::Natural => natural_target_type(target.raw()),
    };
    let target = resolve_target_spec(connection, target, target_type, false, false)?;
    match (kind, target) {
        (HookTargetKind::Pane, Target::Pane(target)) => Ok(ScopeSelector::Pane(target)),
        (HookTargetKind::Pane, _) => Err(ExitFailure::new(
            1,
            format!("{command} -p requires a pane target"),
        )),
        (HookTargetKind::Window, Target::Session(session_name)) => {
            Ok(ScopeSelector::Window(WindowTarget::new(session_name)))
        }
        (HookTargetKind::Window, Target::Window(target)) => Ok(ScopeSelector::Window(target)),
        (HookTargetKind::Window, Target::Pane(target)) => Ok(ScopeSelector::Window(
            WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        )),
        (HookTargetKind::Natural, Target::Session(session_name)) => {
            Ok(ScopeSelector::Session(session_name))
        }
        (HookTargetKind::Natural, Target::Window(target)) => Ok(ScopeSelector::Window(target)),
        (HookTargetKind::Natural, Target::Pane(target)) => Ok(ScopeSelector::Pane(target)),
    }
}

fn validate_hook_registration(hook: HookName, scope: &ScopeSelector) -> Result<(), ExitFailure> {
    rmux_core::validate_hook_registration(hook, scope)
        .map_err(|error| ExitFailure::new(1, error.to_string()))
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

fn reject_target(
    command: &str,
    target: Option<&TargetSpec>,
    flag: &str,
) -> Result<(), ExitFailure> {
    if target.is_some() {
        Err(ExitFailure::new(
            1,
            format!("{command} {flag} does not accept a target"),
        ))
    } else {
        Ok(())
    }
}
