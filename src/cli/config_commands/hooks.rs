use std::path::Path;

use rmux_client::Connection;
use rmux_proto::{
    HookLifecycle, HookName, ResolveTargetType, ScopeSelector, SessionName, Target, WindowTarget,
};

use crate::cli::{
    expect_command_output, resolve_current_pane_target, resolve_current_session_target,
    resolve_target_spec, run_command_resolved, run_payload_command_resolved, ExitFailure,
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
    let scope = resolve_hook_scope(ResolveHookScopeInput {
        command: "set-hook",
        global,
        window,
        pane,
        target,
        hook: Some(hook.hook),
        run_immediately,
        allow_global_target: true,
    })?;

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
    let hook = args.hook;
    let scope = resolve_show_hooks_scope(args.global, args.window, args.pane, args.target, hook)?;
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

struct ResolveHookScopeInput<'a> {
    command: &'a str,
    global: bool,
    window: bool,
    pane: bool,
    target: Option<TargetSpec>,
    hook: Option<HookName>,
    run_immediately: bool,
    allow_global_target: bool,
}

fn resolve_hook_scope(input: ResolveHookScopeInput<'_>) -> Result<HookScope, ExitFailure> {
    let ResolveHookScopeInput {
        command,
        global,
        window,
        pane,
        target,
        hook,
        run_immediately,
        allow_global_target,
    } = input;
    if run_immediately {
        return Ok(
            target.map_or(HookScope::CurrentPane, |target| HookScope::Unresolved {
                target,
                kind: HookTargetKind::Pane,
            }),
        );
    }
    if window && pane {
        return Err(ExitFailure::new(
            1,
            format!("{command} does not support combining -w and -p"),
        ));
    }

    if global {
        if !allow_global_target {
            reject_target(command, target.as_ref(), "-g")?;
        }
        return Ok(target.map_or(
            HookScope::Resolved(ScopeSelector::Global),
            HookScope::TargetCheckedGlobal,
        ));
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
            kind: HookTargetKind::Natural(hook),
        }),
        (false, false, None) => {
            Ok(hook.map_or(HookScope::CurrentSession, HookScope::CurrentNatural))
        }
        (true, true, _) => unreachable!("validated conflicting hook scope flags"),
    }
}

fn resolve_show_hooks_scope(
    global: bool,
    window: bool,
    pane: bool,
    target: Option<TargetSpec>,
    hook: Option<HookName>,
) -> Result<ShowHooksScope, ExitFailure> {
    if global {
        reject_target("show-hooks", target.as_ref(), "-g")?;
        return Ok(ShowHooksScope(HookScope::Resolved(ScopeSelector::Global)));
    }

    resolve_hook_scope(ResolveHookScopeInput {
        command: "show-hooks",
        global: false,
        window,
        pane,
        target,
        hook,
        run_immediately: false,
        allow_global_target: false,
    })
    .map(ShowHooksScope)
}

#[derive(Debug, Clone)]
struct ShowHooksScope(HookScope);

#[derive(Debug, Clone)]
enum HookScope {
    Resolved(ScopeSelector),
    TargetCheckedGlobal(TargetSpec),
    CurrentSession,
    CurrentNatural(HookName),
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
    Natural(Option<HookName>),
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
            Self::TargetCheckedGlobal(target) => {
                let _ = resolve_target_spec(
                    connection,
                    &target,
                    ResolveTargetType::Pane,
                    false,
                    false,
                )?;
                Ok(ScopeSelector::Global)
            }
            Self::CurrentSession => {
                resolve_current_session_target(connection).map(ScopeSelector::Session)
            }
            Self::CurrentNatural(hook) => {
                let session_name = resolve_current_session_target(connection)?;
                resolve_natural_hook_scope_for_session_target(
                    connection,
                    command,
                    hook,
                    session_name,
                )
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
    let target = resolve_target_spec(connection, target, ResolveTargetType::Pane, false, false)?;
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
        (HookTargetKind::Natural(Some(hook)), Target::Session(session_name)) => {
            resolve_natural_hook_scope_for_session_target(connection, command, hook, session_name)
        }
        (HookTargetKind::Natural(Some(hook)), target) => {
            Ok(rmux_core::hook_natural_scope_for_target(hook, target))
        }
        (HookTargetKind::Natural(None), Target::Session(session_name)) => {
            Ok(ScopeSelector::Session(session_name))
        }
        (HookTargetKind::Natural(None), Target::Window(target)) => {
            Ok(ScopeSelector::Session(target.session_name().clone()))
        }
        (HookTargetKind::Natural(None), Target::Pane(target)) => {
            Ok(ScopeSelector::Session(target.session_name().clone()))
        }
    }
}

fn resolve_natural_hook_scope_for_session_target(
    connection: &mut Connection,
    command: &str,
    hook: HookName,
    session_name: SessionName,
) -> Result<ScopeSelector, ExitFailure> {
    if matches!(
        rmux_core::hook_global_root(hook),
        rmux_core::HookGlobalRoot::Session
    ) {
        return Ok(ScopeSelector::Session(session_name));
    }
    let window_index = resolve_active_window_index(connection, &session_name, command)?;
    let pane_index = resolve_active_pane_index(connection, &session_name, window_index, command)?;
    Ok(rmux_core::hook_natural_scope_for_session_target(
        hook,
        session_name,
        window_index,
        pane_index,
    ))
}

fn resolve_active_window_index(
    connection: &mut Connection,
    session_name: &SessionName,
    command: &str,
) -> Result<u32, ExitFailure> {
    let response = connection
        .list_windows(
            session_name.clone(),
            Some("#{window_index}:#{window_active}".to_owned()),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    let stdout = String::from_utf8_lossy(output.stdout());
    for line in stdout.lines() {
        let Some((index, active)) = line.split_once(':') else {
            continue;
        };
        if active == "1" {
            return index.parse::<u32>().map_err(|error| {
                ExitFailure::new(1, format!("{command}: invalid active window: {error}"))
            });
        }
    }
    Err(ExitFailure::new(
        1,
        format!("{command}: no active window in session {session_name}"),
    ))
}

fn resolve_active_pane_index(
    connection: &mut Connection,
    session_name: &SessionName,
    window_index: u32,
    command: &str,
) -> Result<u32, ExitFailure> {
    let response = connection
        .list_panes_in_window(
            session_name.clone(),
            Some(window_index),
            Some("#{pane_index}:#{pane_active}".to_owned()),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-panes")?;
    let stdout = String::from_utf8_lossy(output.stdout());
    for line in stdout.lines() {
        let Some((index, active)) = line.split_once(':') else {
            continue;
        };
        if active == "1" {
            return index.parse::<u32>().map_err(|error| {
                ExitFailure::new(1, format!("{command}: invalid active pane: {error}"))
            });
        }
    }
    Err(ExitFailure::new(
        1,
        format!("{command}: no active pane in session {session_name}:{window_index}"),
    ))
}

fn validate_hook_registration(hook: HookName, scope: &ScopeSelector) -> Result<(), ExitFailure> {
    rmux_core::validate_hook_registration(hook, scope)
        .map_err(|error| ExitFailure::new(1, error.to_string()))
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
