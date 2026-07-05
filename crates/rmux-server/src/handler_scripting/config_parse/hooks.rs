use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::request::Request;
use rmux_proto::{
    HookLifecycle, HookName, RmuxError, ScopeSelector, SetHookMutationRequest, ShowHooksRequest,
    Target, WindowTarget,
};

use super::super::parse_target_arg;
use super::super::targets::implicit_session_name;
use super::super::tokens::CommandTokens;

pub(in crate::handler::scripting_support) fn parse_set_hook(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut global = false;
    let mut window = false;
    let mut pane = false;
    let mut append = false;
    let mut run_immediately = false;
    let mut unset = false;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" => {
                let _ = args.optional();
                append = true;
            }
            "-g" => {
                let _ = args.optional();
                global = true;
            }
            "-p" => {
                let _ = args.optional();
                pane = true;
            }
            "-R" => {
                let _ = args.optional();
                run_immediately = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_target_arg("set-hook", args.required("-t target")?)?);
            }
            "-u" => {
                let _ = args.optional();
                unset = true;
            }
            "-w" => {
                let _ = args.optional();
                window = true;
            }
            token if compact_hook_target_flag_prefix(token, true).is_some() => {
                let token = args.optional().expect("peeked token exists");
                let prefix = compact_hook_target_flag_prefix(&token, true)
                    .expect("validated compact set-hook target flag");
                apply_set_hook_flag_cluster(
                    &prefix,
                    &mut global,
                    &mut window,
                    &mut pane,
                    &mut append,
                    &mut run_immediately,
                    &mut unset,
                );
                target = Some(parse_target_arg("set-hook", args.required("-t target")?)?);
            }
            token if is_compact_hook_flag_cluster(token, true) => {
                let token = args.optional().expect("peeked token exists");
                apply_set_hook_flag_cluster(
                    &token,
                    &mut global,
                    &mut window,
                    &mut pane,
                    &mut append,
                    &mut run_immediately,
                    &mut unset,
                );
            }
            _ => break,
        }
    }

    let hook = parse_hook_spec(&args.required("set-hook hook")?)?;
    let scope = resolve_hook_scope(HookScopeRequest {
        command: "set-hook",
        hook: hook.hook,
        global,
        window,
        pane,
        target,
        run_immediately,
        sessions,
        find_context,
    })?;
    let command = if run_immediately || unset {
        args.optional()
    } else {
        Some(args.required("set-hook command")?)
    };
    args.no_extra("set-hook")?;

    Ok(Request::SetHookMutation(SetHookMutationRequest {
        scope,
        hook: hook.hook,
        command,
        lifecycle: HookLifecycle::Persistent,
        append,
        unset,
        run_immediately,
        index: hook.index,
    }))
}

pub(in crate::handler::scripting_support) fn parse_show_hooks(
    mut args: CommandTokens,
) -> Result<Request, RmuxError> {
    let mut global = false;
    let mut window = false;
    let mut pane = false;
    let mut target = None;

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
            "-p" => {
                let _ = args.optional();
                pane = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_target_arg("show-hooks", args.required("-t target")?)?);
            }
            "-w" => {
                let _ = args.optional();
                window = true;
            }
            token if compact_hook_target_flag_prefix(token, false).is_some() => {
                let token = args.optional().expect("peeked token exists");
                let prefix = compact_hook_target_flag_prefix(&token, false)
                    .expect("validated compact show-hooks target flag");
                apply_show_hooks_flag_cluster(&prefix, &mut global, &mut window, &mut pane);
                target = Some(parse_target_arg("show-hooks", args.required("-t target")?)?);
            }
            token if is_compact_hook_flag_cluster(token, false) => {
                let token = args.optional().expect("peeked token exists");
                apply_show_hooks_flag_cluster(&token, &mut global, &mut window, &mut pane);
            }
            _ => break,
        }
    }

    let scope = resolve_show_hooks_scope(global, window, pane, target)?;
    let hook = args
        .optional()
        .map(|value| parse_hook_name(&value))
        .transpose()?;
    args.no_extra("show-hooks")?;

    Ok(Request::ShowHooks(ShowHooksRequest {
        scope,
        window,
        pane,
        hook,
    }))
}

fn is_compact_hook_flag_cluster(token: &str, set_hook: bool) -> bool {
    let Some(flags) = token.strip_prefix('-') else {
        return false;
    };
    if flags.len() <= 1 || flags.starts_with('-') {
        return false;
    }
    flags
        .chars()
        .all(|flag| matches!(flag, 'g' | 'p' | 'w') || set_hook && matches!(flag, 'a' | 'R' | 'u'))
}

fn compact_hook_target_flag_prefix(token: &str, set_hook: bool) -> Option<String> {
    let flags = token.strip_prefix('-')?;
    if flags.len() <= 1 || flags.starts_with('-') || !flags.ends_with('t') {
        return None;
    }
    let prefix = &flags[..flags.len() - 1];
    if prefix.is_empty()
        || !prefix.chars().all(|flag| {
            matches!(flag, 'g' | 'p' | 'w') || set_hook && matches!(flag, 'a' | 'R' | 'u')
        })
    {
        return None;
    }
    Some(format!("-{prefix}"))
}

fn apply_set_hook_flag_cluster(
    token: &str,
    global: &mut bool,
    window: &mut bool,
    pane: &mut bool,
    append: &mut bool,
    run_immediately: &mut bool,
    unset: &mut bool,
) {
    for flag in token
        .strip_prefix('-')
        .expect("compact flag cluster starts with '-'")
        .chars()
    {
        match flag {
            'a' => *append = true,
            'g' => *global = true,
            'p' => *pane = true,
            'R' => *run_immediately = true,
            'u' => *unset = true,
            'w' => *window = true,
            _ => unreachable!("validated compact set-hook flag"),
        }
    }
}

fn apply_show_hooks_flag_cluster(
    token: &str,
    global: &mut bool,
    window: &mut bool,
    pane: &mut bool,
) {
    for flag in token
        .strip_prefix('-')
        .expect("compact flag cluster starts with '-'")
        .chars()
    {
        match flag {
            'g' => *global = true,
            'p' => *pane = true,
            'w' => *window = true,
            _ => unreachable!("validated compact show-hooks flag"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedHookSpec {
    hook: HookName,
    index: Option<u32>,
}

struct HookScopeRequest<'a> {
    command: &'a str,
    hook: HookName,
    global: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
    run_immediately: bool,
    sessions: &'a SessionStore,
    find_context: &'a TargetFindContext,
}

fn resolve_hook_scope(request: HookScopeRequest<'_>) -> Result<ScopeSelector, RmuxError> {
    let HookScopeRequest {
        command,
        hook,
        global,
        window,
        pane,
        target,
        run_immediately,
        sessions,
        find_context,
    } = request;
    if window && pane {
        return Err(RmuxError::Server(format!(
            "{command} does not support combining -w and -p"
        )));
    }

    if global {
        return Ok(ScopeSelector::Global);
    }

    let scope = match (window, pane, target) {
        (true, false, Some(Target::Session(session_name))) => {
            Ok(ScopeSelector::Window(WindowTarget::new(session_name)))
        }
        (true, false, Some(Target::Window(target))) => Ok(ScopeSelector::Window(target)),
        (true, false, Some(Target::Pane(target))) => Ok(ScopeSelector::Window(
            WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        )),
        (true, false, None) => Err(RmuxError::Server(format!("{command} -w requires a target"))),
        (false, true, Some(Target::Pane(target))) => Ok(ScopeSelector::Pane(target)),
        (false, true, Some(_)) => Err(RmuxError::Server(format!(
            "{command} -p requires a pane target"
        ))),
        (false, true, None) => Err(RmuxError::Server(format!("{command} -p requires a target"))),
        (false, false, Some(target)) => resolve_natural_hook_scope(command, hook, target, sessions),
        (false, false, None) if run_immediately && sessions.is_empty() => Ok(ScopeSelector::Global),
        (false, false, None) if run_immediately => Ok(ScopeSelector::Session(
            implicit_session_name(sessions, find_context, command)?,
        )),
        (false, false, None) if sessions.is_empty() => Ok(ScopeSelector::Global),
        (false, false, None) => Ok(ScopeSelector::Session(implicit_session_name(
            sessions,
            find_context,
            command,
        )?)),
        (true, true, _) => unreachable!("validated conflicting hook scope flags"),
    }?;
    rmux_core::validate_hook_registration(hook, &scope)?;
    Ok(scope)
}

fn resolve_natural_hook_scope(
    command: &str,
    hook: HookName,
    target: Target,
    sessions: &SessionStore,
) -> Result<ScopeSelector, RmuxError> {
    let Target::Session(session_name) = target else {
        return Ok(rmux_core::hook_natural_scope_for_target(hook, target));
    };

    let session = sessions
        .session(&session_name)
        .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
    let window_index = session.active_window_index();
    let pane_index = session.active_pane_index();
    let target = rmux_core::hook_natural_scope_for_session_target(
        hook,
        session_name,
        window_index,
        pane_index,
    );
    rmux_core::validate_hook_registration(hook, &target).map_err(|error| {
        RmuxError::Server(format!(
            "{command} cannot register {}: {error}",
            hook.as_str()
        ))
    })?;
    Ok(target)
}

fn resolve_show_hooks_scope(
    global: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
) -> Result<ScopeSelector, RmuxError> {
    if global {
        if target.is_some() {
            return Err(RmuxError::Server(
                "show-hooks -g does not accept a target".to_owned(),
            ));
        }
        return Ok(ScopeSelector::Global);
    }

    if window && pane {
        return Err(RmuxError::Server(
            "show-hooks does not support combining -w and -p".to_owned(),
        ));
    }

    match (window, pane, target) {
        (true, false, Some(Target::Session(session_name))) => {
            Ok(ScopeSelector::Window(WindowTarget::new(session_name)))
        }
        (true, false, Some(Target::Window(target))) => Ok(ScopeSelector::Window(target)),
        (true, false, Some(Target::Pane(target))) => Ok(ScopeSelector::Window(
            WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        )),
        (true, false, None) => Err(RmuxError::Server(
            "show-hooks -w requires a target".to_owned(),
        )),
        (false, true, Some(Target::Pane(target))) => Ok(ScopeSelector::Pane(target)),
        (false, true, Some(_)) => Err(RmuxError::Server(
            "show-hooks -p requires a pane target".to_owned(),
        )),
        (false, true, None) => Err(RmuxError::Server(
            "show-hooks -p requires a target".to_owned(),
        )),
        (false, false, Some(Target::Session(session_name))) => {
            Ok(ScopeSelector::Session(session_name))
        }
        (false, false, Some(Target::Window(target))) => Ok(ScopeSelector::Window(target)),
        (false, false, Some(Target::Pane(target))) => Ok(ScopeSelector::Pane(target)),
        (false, false, None) => Err(RmuxError::Server(
            "show-hooks requires -g or a target".to_owned(),
        )),
        (true, true, _) => unreachable!("validated conflicting show-hooks scope flags"),
    }
}

fn parse_hook_spec(value: &str) -> Result<ParsedHookSpec, RmuxError> {
    let (name, index) = if let Some(open_bracket) = value.find('[') {
        let Some(index_text) = value[open_bracket + 1..].strip_suffix(']') else {
            return Err(RmuxError::Server(format!("unknown hook: {value}")));
        };
        let index = index_text
            .parse::<u32>()
            .map_err(|_| RmuxError::Server(format!("invalid hook index: {value}")))?;
        (&value[..open_bracket], Some(index))
    } else {
        (value, None)
    };

    Ok(ParsedHookSpec {
        hook: parse_hook_name(name)?,
        index,
    })
}

fn parse_hook_name(value: &str) -> Result<HookName, RmuxError> {
    HookName::from_str(value).ok_or_else(|| RmuxError::Server(format!("unknown hook: {value}")))
}
