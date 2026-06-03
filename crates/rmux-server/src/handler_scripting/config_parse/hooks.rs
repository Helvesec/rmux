use rmux_proto::request::Request;
use rmux_proto::{
    HookLifecycle, HookName, RmuxError, ScopeSelector, SetHookMutationRequest, ShowHooksRequest,
    Target, WindowTarget,
};

use super::super::parse_target_arg;
use super::super::tokens::CommandTokens;

pub(in crate::handler::scripting_support) fn parse_set_hook(
    mut args: CommandTokens,
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
            _ => break,
        }
    }

    let hook = parse_hook_spec(&args.required("set-hook hook")?)?;
    let scope = resolve_hook_scope("set-hook", hook.hook, global, window, pane, target)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedHookSpec {
    hook: HookName,
    index: Option<u32>,
}

fn resolve_hook_scope(
    command: &str,
    hook: HookName,
    global: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
) -> Result<ScopeSelector, RmuxError> {
    if window && pane {
        return Err(RmuxError::Server(format!(
            "{command} does not support combining -w and -p"
        )));
    }

    if global {
        if target.is_some() {
            return Err(RmuxError::Server(format!(
                "{command} -g does not accept a target"
            )));
        }
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
        (false, false, Some(target)) => resolve_implicit_hook_target_scope(hook, target),
        (false, false, None) => Err(RmuxError::Server(format!(
            "{command} requires -g or a target"
        ))),
        (true, true, _) => unreachable!("validated conflicting hook scope flags"),
    }?;
    rmux_core::validate_hook_registration(hook, &scope)?;
    Ok(scope)
}

fn resolve_implicit_hook_target_scope(
    hook: HookName,
    target: Target,
) -> Result<ScopeSelector, RmuxError> {
    let candidates = match target {
        Target::Session(session_name) => vec![ScopeSelector::Session(session_name)],
        Target::Window(target) => vec![
            ScopeSelector::Window(target.clone()),
            ScopeSelector::Session(target.session_name().clone()),
        ],
        Target::Pane(target) => vec![
            ScopeSelector::Pane(target.clone()),
            ScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )),
            ScopeSelector::Session(target.session_name().clone()),
        ],
    };

    let mut first_error = None;
    for scope in candidates {
        match rmux_core::validate_hook_registration(hook, &scope) {
            Ok(()) => return Ok(scope),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    Err(first_error.unwrap_or_else(|| RmuxError::Server("invalid hook target".to_owned())))
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
