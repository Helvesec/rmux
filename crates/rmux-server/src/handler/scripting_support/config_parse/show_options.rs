//! `show-options` parsing and the scope it reads from flags, target and option name.

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::request::Request;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{RmuxError, ScopeSelector, ShowOptionsRequest, Target, WindowTarget};

use super::super::parse_target_arg;
use super::super::targets::{implicit_pane_target, implicit_session_name, implicit_window_target};
use super::super::tokens::CommandTokens;
use super::super::values::{reject_unknown_option_before_positional, unsupported_flag};
use super::dummy_pane_scope;

pub(in crate::handler::scripting_support) fn parse_show_options(
    mut args: CommandTokens,
    force_window: bool,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let command_name = if force_window {
        "show-window-options"
    } else {
        "show-options"
    };
    let mut global = false;
    let mut server = false;
    let mut window = force_window;
    let mut pane = false;
    let mut value_only = false;
    let mut include_inherited = false;
    let mut include_hooks = false;
    let mut quiet = false;
    let mut target = None;
    let mut name = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-g" => {
                let _ = args.optional();
                global = true;
            }
            "-s" => {
                if force_window {
                    return Err(unsupported_flag(command_name, "-s"));
                }
                let _ = args.optional();
                server = true;
            }
            "-w" => {
                if force_window {
                    return Err(unsupported_flag(command_name, "-w"));
                }
                let _ = args.optional();
                window = true;
            }
            "-p" => {
                if force_window {
                    return Err(unsupported_flag(command_name, "-p"));
                }
                let _ = args.optional();
                pane = true;
            }
            "-v" => {
                let _ = args.optional();
                value_only = true;
            }
            "-A" => {
                if force_window {
                    return Err(unsupported_flag(command_name, "-A"));
                }
                let _ = args.optional();
                include_inherited = true;
            }
            "-H" if force_window => return Err(unsupported_flag(command_name, "-H")),
            "-H" => {
                let _ = args.optional();
                include_hooks = true;
            }
            "-q" if force_window => return Err(unsupported_flag(command_name, "-q")),
            "-q" => {
                let _ = args.optional();
                quiet = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_target_arg(command_name, args.required("-t target")?)?);
            }
            token if is_show_options_flag_cluster(token) => {
                let flags = args
                    .optional()
                    .expect("peeked show-options flag cluster must be present");
                for flag in flags[1..].chars() {
                    match flag {
                        'g' => global = true,
                        's' if !force_window => server = true,
                        'w' if !force_window => window = true,
                        'p' if !force_window => pane = true,
                        'v' => value_only = true,
                        'A' if !force_window => include_inherited = true,
                        'A' => return Err(unsupported_flag(command_name, "-A")),
                        'H' if !force_window => include_hooks = true,
                        'H' => return Err(unsupported_flag(command_name, "-H")),
                        'q' if force_window => return Err(unsupported_flag(command_name, "-q")),
                        'q' => quiet = true,
                        's' => return Err(unsupported_flag(command_name, "-s")),
                        'w' => return Err(unsupported_flag(command_name, "-w")),
                        'p' => return Err(unsupported_flag(command_name, "-p")),
                        _ => return Err(unsupported_flag(command_name, &format!("-{flag}"))),
                    }
                }
            }
            _ => {
                reject_unknown_option_before_positional(command_name, token)?;
                break;
            }
        }
    }

    if let Some(argument) = args.optional() {
        name = Some(argument);
    }
    args.no_extra(command_name)?;
    let scope = resolve_show_options_scope(ShowOptionsScopeRequest {
        command_name,
        global,
        server,
        window,
        pane,
        target,
        name: name.as_deref(),
        quiet,
        sessions,
        find_context,
    })?;

    Ok(Request::ShowOptions(ShowOptionsRequest {
        scope,
        name,
        value_only,
        include_inherited,
        quiet,
        include_hooks,
    }))
}

fn is_show_options_flag_cluster(token: &str) -> bool {
    token.starts_with('-')
        && !token.starts_with("--")
        && token.len() > 2
        && token[1..]
            .chars()
            .all(|flag| matches!(flag, 'A' | 'H' | 'g' | 's' | 'w' | 'p' | 'v' | 'q'))
}

struct ShowOptionsScopeRequest<'a> {
    command_name: &'a str,
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
    name: Option<&'a str>,
    quiet: bool,
    sessions: &'a ManagedSessionStore,
    find_context: &'a TargetFindContext,
}

fn resolve_show_options_scope(
    request: ShowOptionsScopeRequest<'_>,
) -> Result<OptionScopeSelector, RmuxError> {
    let ShowOptionsScopeRequest {
        command_name: command,
        global,
        server,
        window,
        pane,
        target,
        name,
        quiet,
        sessions,
        find_context,
    } = request;
    if [server, window, pane]
        .into_iter()
        .filter(|flag| *flag)
        .count()
        > 1
    {
        return Err(RmuxError::Server(format!(
            "{command} accepts at most one of -s, -w, or -p"
        )));
    }

    let hook = name.and_then(show_options_hook_name);
    if !global && !server && !window && !pane {
        if let Some(hook) = hook {
            return natural_hook_option_scope(hook, target, sessions, find_context, command);
        }
    }

    if !global {
        if let Some(name) = name {
            if let Some(scope) = natural_known_show_options_scope(
                name,
                pane,
                target.clone(),
                sessions,
                find_context,
                command,
            )? {
                return Ok(scope);
            }
        }
    }

    if server {
        return Ok(OptionScopeSelector::ServerGlobal);
    }

    match (window, pane, target) {
        (true, false, _) if global => Ok(OptionScopeSelector::WindowGlobal),
        (true, false, Some(Target::Session(session_name))) => {
            Ok(OptionScopeSelector::Window(WindowTarget::new(session_name)))
        }
        (true, false, Some(Target::Window(target))) => Ok(OptionScopeSelector::Window(target)),
        (true, false, Some(Target::Pane(target))) => Ok(OptionScopeSelector::Window(
            WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        )),
        (true, false, None) => Ok(OptionScopeSelector::Window(implicit_window_target(
            sessions,
            find_context,
            command,
        )?)),
        (false, true, target) if global => resolve_show_options_global_pane_scope(
            name,
            quiet,
            target,
            sessions,
            find_context,
            command,
        ),
        (false, true, Some(Target::Pane(target))) => Ok(OptionScopeSelector::Pane(target)),
        (false, true, Some(_)) => Err(RmuxError::Server(format!(
            "{command} -p requires a pane target"
        ))),
        (false, true, None) => Ok(OptionScopeSelector::Pane(implicit_pane_target(
            sessions,
            find_context,
            command,
        )?)),
        (false, false, _) if global => resolve_show_options_global_scope(name, quiet),
        (false, false, Some(Target::Session(session_name))) => {
            Ok(OptionScopeSelector::Session(session_name))
        }
        (false, false, Some(Target::Window(target))) => Ok(OptionScopeSelector::Window(target)),
        (false, false, Some(Target::Pane(target))) => Ok(OptionScopeSelector::Pane(target)),
        (false, false, None) => Ok(OptionScopeSelector::Session(implicit_session_name(
            sessions,
            find_context,
            command,
        )?)),
        (true, true, _) => unreachable!("validated conflicting show-options scope flags"),
    }
}

fn resolve_show_options_global_scope(
    name: Option<&str>,
    quiet: bool,
) -> Result<OptionScopeSelector, RmuxError> {
    let Some(name) = name else {
        return Ok(OptionScopeSelector::SessionGlobal);
    };
    if let Some(hook) = show_options_hook_name(name) {
        return Ok(global_hook_option_scope(hook));
    }
    match rmux_core::default_global_scope_for_option_name(name) {
        Ok(scope) => Ok(scope),
        Err(error) if quiet && show_options_quiet_suppresses(&error) => {
            Ok(OptionScopeSelector::SessionGlobal)
        }
        Err(error) => Err(error),
    }
}

fn resolve_show_options_global_pane_scope(
    name: Option<&str>,
    quiet: bool,
    target: Option<Target>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command: &str,
) -> Result<OptionScopeSelector, RmuxError> {
    if let Some(name) = name {
        if show_options_hook_name(name).is_some() {
            return resolve_show_options_pane_scope(target, sessions, find_context, command);
        }
        match rmux_core::resolve_option_name(name) {
            Ok(query) if query.is_user() || query.supports_scope(&dummy_pane_scope()) => {
                return resolve_show_options_pane_scope(target, sessions, find_context, command);
            }
            Ok(_) => return resolve_show_options_global_scope(Some(name), quiet),
            Err(error) if quiet && show_options_quiet_suppresses(&error) => {
                return resolve_show_options_pane_scope(target, sessions, find_context, command);
            }
            Err(error) => return Err(error),
        }
    }

    resolve_show_options_pane_scope(target, sessions, find_context, command)
}

fn resolve_show_options_pane_scope(
    target: Option<Target>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command: &str,
) -> Result<OptionScopeSelector, RmuxError> {
    match target {
        Some(Target::Pane(target)) => Ok(OptionScopeSelector::Pane(target)),
        Some(_) => Err(RmuxError::Server(format!(
            "{command} -p requires a pane target"
        ))),
        None => Ok(OptionScopeSelector::Pane(implicit_pane_target(
            sessions,
            find_context,
            command,
        )?)),
    }
}

fn natural_known_show_options_scope(
    option: &str,
    pane: bool,
    target: Option<Target>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command: &str,
) -> Result<Option<OptionScopeSelector>, RmuxError> {
    let Ok(query) = rmux_core::resolve_option_name(option) else {
        return Ok(None);
    };
    if query.is_user() {
        return Ok(None);
    }

    let server_scope = OptionScopeSelector::ServerGlobal;
    if query.supports_scope(&server_scope) {
        return Ok(Some(server_scope));
    }

    let session_scope = OptionScopeSelector::SessionGlobal;
    if query.supports_scope(&session_scope) {
        let session_name = match target {
            Some(Target::Session(session_name)) => session_name,
            Some(Target::Window(target)) => target.session_name().clone(),
            Some(Target::Pane(target)) => target.session_name().clone(),
            None => implicit_session_name(sessions, find_context, command)?,
        };
        return Ok(Some(OptionScopeSelector::Session(session_name)));
    }

    if pane {
        let pane_target = match target.clone() {
            Some(Target::Pane(target)) => target,
            Some(_) => {
                return Err(RmuxError::Server(format!(
                    "{command} -p requires a pane target"
                )));
            }
            None => implicit_pane_target(sessions, find_context, command)?,
        };
        let pane_scope = OptionScopeSelector::Pane(pane_target);
        if query.supports_scope(&pane_scope) {
            return Ok(Some(pane_scope));
        }
    }

    let window_target = match target {
        Some(Target::Session(session_name)) => WindowTarget::new(session_name),
        Some(Target::Window(target)) => target,
        Some(Target::Pane(target)) => {
            WindowTarget::with_window(target.session_name().clone(), target.window_index())
        }
        None => implicit_window_target(sessions, find_context, command)?,
    };
    let window_scope = OptionScopeSelector::Window(window_target);
    if query.supports_scope(&window_scope) {
        return Ok(Some(window_scope));
    }

    Ok(None)
}

fn show_options_quiet_suppresses(error: &RmuxError) -> bool {
    let message = match error {
        RmuxError::Server(message) | RmuxError::Message(message) => message.as_str(),
        _ => return false,
    };
    show_options_lookup_error(message)
}

fn show_options_lookup_error(message: &str) -> bool {
    message.starts_with("unknown option: ")
        || message.starts_with("invalid option: ")
        || message.starts_with("ambiguous option: ")
}

fn show_options_hook_name(value: &str) -> Option<rmux_proto::HookName> {
    let name = match value.rsplit_once('[') {
        Some((name, index)) if index.strip_suffix(']')?.parse::<u32>().is_ok() => name,
        Some(_) => return None,
        None => value,
    };
    rmux_proto::HookName::from_str(name)
}

fn global_hook_option_scope(hook: rmux_proto::HookName) -> OptionScopeSelector {
    match rmux_core::hook_global_root(hook) {
        rmux_core::HookGlobalRoot::Session => OptionScopeSelector::SessionGlobal,
        rmux_core::HookGlobalRoot::Window => OptionScopeSelector::WindowGlobal,
    }
}

fn natural_hook_option_scope(
    hook: rmux_proto::HookName,
    target: Option<Target>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command: &str,
) -> Result<OptionScopeSelector, RmuxError> {
    let target = match target {
        Some(Target::Session(session_name)) => {
            let session = sessions
                .session(&session_name)
                .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
            rmux_core::hook_natural_scope_for_session_target(
                hook,
                session_name,
                session.active_window_index(),
                session.active_pane_index(),
            )
        }
        Some(target) => rmux_core::hook_natural_scope_for_target(hook, target),
        None => {
            let session_name = implicit_session_name(sessions, find_context, command)?;
            let session = sessions
                .session(&session_name)
                .expect("implicit session exists");
            rmux_core::hook_natural_scope_for_session_target(
                hook,
                session_name,
                session.active_window_index(),
                session.active_pane_index(),
            )
        }
    };
    Ok(match target {
        ScopeSelector::Global => unreachable!("natural hook scope is local"),
        ScopeSelector::Session(session_name) => OptionScopeSelector::Session(session_name),
        ScopeSelector::Window(target) => OptionScopeSelector::Window(target),
        ScopeSelector::Pane(target) => OptionScopeSelector::Pane(target),
    })
}
