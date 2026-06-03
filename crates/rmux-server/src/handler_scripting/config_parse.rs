use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::request::Request;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    RmuxError, ScopeSelector, SessionName, SetEnvironmentMode, SetEnvironmentRequest,
    SetOptionByNameRequest, SetOptionMode, ShowEnvironmentRequest, ShowOptionsRequest, Target,
    WindowTarget,
};

use super::targets::implicit_pane_target;
use super::tokens::CommandTokens;
use super::values::unsupported_flag;
use super::{parse_session_name, parse_target_arg};

#[path = "config_parse/hooks.rs"]
mod hooks;

pub(super) use hooks::{parse_set_hook, parse_show_hooks};

pub(super) enum ParsedSetOptionCommand {
    Request(Box<Request>),
    NoOp,
}

pub(super) fn parse_set_option(
    args: CommandTokens,
    force_window: bool,
    default_target: Option<Target>,
) -> Result<Request, RmuxError> {
    match parse_set_option_invocation(args, force_window, default_target)? {
        ParsedSetOptionCommand::Request(request) => Ok(*request),
        ParsedSetOptionCommand::NoOp => Err(RmuxError::Server(
            "server scope is not supported for this option".to_owned(),
        )),
    }
}

pub(super) fn parse_set_option_invocation(
    mut args: CommandTokens,
    force_window: bool,
    default_target: Option<Target>,
) -> Result<ParsedSetOptionCommand, RmuxError> {
    let command_name = if force_window {
        "set-window-option"
    } else {
        "set-option"
    };
    let mut flags = SetOptionFlags::new(force_window);
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-g" => {
                let _ = args.optional();
                flags.global = true;
            }
            "-s" => {
                let _ = args.optional();
                flags.server = true;
            }
            "-w" if !force_window => {
                let _ = args.optional();
                flags.window = true;
            }
            "-p" if !force_window => {
                let _ = args.optional();
                flags.pane = true;
            }
            "-q" => {
                let _ = args.optional();
            }
            "-w" if force_window => {
                let _ = args.optional();
            }
            "-a" => {
                let _ = args.optional();
                flags.append = true;
            }
            "-o" => {
                let _ = args.optional();
                flags.only_if_unset = true;
            }
            "-u" => {
                let _ = args.optional();
                flags.unset = true;
            }
            "-U" if !force_window => {
                let _ = args.optional();
                flags.unset_pane_overrides = true;
                flags.unset = true;
                flags.window = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_target_arg("set-option", args.required("-t target")?)?);
            }
            token if is_set_option_flag_cluster(token, force_window) => {
                let token = args
                    .optional()
                    .expect("peeked set-option flag cluster must be present");
                flags.apply_cluster(command_name, &token)?;
            }
            _ => break,
        }
    }

    if flags.scope_count() > 1 {
        return Err(RmuxError::Server(
            "set-option accepts at most one of -s, -w, or -p".to_owned(),
        ));
    }

    let option = args.required("set-option option")?;
    let value = args.optional();
    args.no_extra("set-option")?;

    let scope = resolve_set_option_scope(
        &option,
        flags.global,
        flags.server,
        flags.window,
        flags.pane,
        flags.append,
        target.or(default_target),
    )?;
    let Some(scope) = scope.into_scope() else {
        return Ok(ParsedSetOptionCommand::NoOp);
    };
    let mode = if flags.append {
        SetOptionMode::Append
    } else {
        SetOptionMode::Replace
    };
    rmux_core::validate_option_name_mutation(&option, &scope, mode, value.as_deref(), flags.unset)?;

    Ok(ParsedSetOptionCommand::Request(Box::new(
        Request::SetOptionByName(SetOptionByNameRequest {
            scope,
            name: option,
            value,
            mode,
            only_if_unset: flags.only_if_unset,
            unset: flags.unset,
            unset_pane_overrides: flags.unset_pane_overrides,
        }),
    )))
}

pub(super) fn default_set_option_target(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Option<Target> {
    implicit_pane_target(sessions, find_context, "set-option")
        .ok()
        .map(Target::Pane)
}

struct SetOptionFlags {
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    append: bool,
    only_if_unset: bool,
    unset: bool,
    unset_pane_overrides: bool,
}

impl SetOptionFlags {
    fn new(force_window: bool) -> Self {
        Self {
            global: false,
            server: false,
            window: force_window,
            pane: false,
            append: false,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
        }
    }

    fn scope_count(&self) -> usize {
        [self.server, self.window, self.pane]
            .into_iter()
            .filter(|flag| *flag)
            .count()
    }

    fn apply_cluster(&mut self, command_name: &str, token: &str) -> Result<(), RmuxError> {
        for flag in token[1..].chars() {
            match flag {
                'g' => self.global = true,
                's' => self.server = true,
                'w' => self.window = true,
                'p' => self.pane = true,
                'q' => {}
                'a' => self.append = true,
                'o' => self.only_if_unset = true,
                'u' => self.unset = true,
                'U' => {
                    self.unset_pane_overrides = true;
                    self.unset = true;
                    self.window = true;
                }
                _ => return Err(unsupported_flag(command_name, &format!("-{flag}"))),
            }
        }
        Ok(())
    }
}

fn is_set_option_flag_cluster(token: &str, force_window: bool) -> bool {
    token.starts_with('-')
        && !token.starts_with("--")
        && token.len() > 2
        && token[1..].chars().all(|flag| {
            matches!(flag, 'g' | 'a' | 'o' | 'q' | 'u')
                || (!force_window && matches!(flag, 's' | 'w' | 'p' | 'U'))
                || (force_window && flag == 'w')
        })
}

pub(super) fn parse_set_environment(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut global = false;
    let mut format = false;
    let mut hidden = false;
    let mut mode = Some(SetEnvironmentMode::Set);
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-F" => {
                let _ = args.optional();
                format = true;
            }
            "-g" => {
                let _ = args.optional();
                global = true;
            }
            "-h" => {
                let _ = args.optional();
                hidden = true;
            }
            "-r" => {
                let _ = args.optional();
                mode = Some(SetEnvironmentMode::Clear);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_session_name(args.required("-t target")?)?);
            }
            "-u" => {
                let _ = args.optional();
                mode = Some(SetEnvironmentMode::Unset);
            }
            _ => break,
        }
    }

    let scope = build_global_or_session_scope("set-environment", global, target)?;
    let name = args.required("set-environment name")?;
    let value = match mode.unwrap_or(SetEnvironmentMode::Set) {
        SetEnvironmentMode::Set => args
            .optional()
            .ok_or_else(|| RmuxError::Server("no value specified".to_owned()))?,
        SetEnvironmentMode::Clear | SetEnvironmentMode::Unset => {
            args.optional().unwrap_or_default()
        }
    };
    args.no_extra("set-environment")?;

    Ok(Request::SetEnvironment(SetEnvironmentRequest {
        scope,
        name,
        value,
        mode,
        hidden,
        format,
    }))
}

pub(super) fn parse_show_options(
    mut args: CommandTokens,
    force_window: bool,
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
                let _ = args.optional();
                include_inherited = true;
            }
            "-q" if force_window => return Err(unsupported_flag(command_name, "-q")),
            "-q" => {
                let _ = args.optional();
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
                        'A' => include_inherited = true,
                        'q' if force_window => return Err(unsupported_flag(command_name, "-q")),
                        'q' => {}
                        's' => return Err(unsupported_flag(command_name, "-s")),
                        'w' => return Err(unsupported_flag(command_name, "-w")),
                        'p' => return Err(unsupported_flag(command_name, "-p")),
                        _ => return Err(unsupported_flag(command_name, &format!("-{flag}"))),
                    }
                }
            }
            _ => break,
        }
    }

    if let Some(argument) = args.optional() {
        name = Some(argument);
    }
    args.no_extra(command_name)?;
    let scope = resolve_show_options_scope(
        command_name,
        global,
        server,
        window,
        pane,
        target,
        name.as_deref(),
    )?;

    Ok(Request::ShowOptions(ShowOptionsRequest {
        scope,
        name,
        value_only,
        include_inherited,
    }))
}

pub(super) fn parse_show_environment(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut global = false;
    let mut hidden = false;
    let mut shell_format = false;
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
            "-h" => {
                let _ = args.optional();
                hidden = true;
            }
            "-s" => {
                let _ = args.optional();
                shell_format = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_session_name(args.required("-t target")?)?);
            }
            flag if flag.starts_with('-') => {
                return Err(unsupported_flag("show-environment", flag));
            }
            _ => break,
        }
    }

    let scope = build_global_or_session_scope("show-environment", global, target)?;
    let name = args.optional();
    args.no_extra("show-environment")?;

    Ok(Request::ShowEnvironment(ShowEnvironmentRequest {
        scope,
        name,
        hidden,
        shell_format,
    }))
}

fn is_show_options_flag_cluster(token: &str) -> bool {
    token.starts_with('-')
        && !token.starts_with("--")
        && token.len() > 2
        && token[1..]
            .chars()
            .all(|flag| matches!(flag, 'A' | 'g' | 's' | 'w' | 'p' | 'v' | 'q'))
}

fn build_global_or_session_scope(
    command: &str,
    global: bool,
    target: Option<SessionName>,
) -> Result<ScopeSelector, RmuxError> {
    match (global, target) {
        (true, None) => Ok(ScopeSelector::Global),
        (false, Some(session_name)) => Ok(ScopeSelector::Session(session_name)),
        _ => Err(RmuxError::Server(format!(
            "{command} requires exactly one of -g or -t target"
        ))),
    }
}

fn resolve_set_option_scope(
    option: &str,
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    append: bool,
    target: Option<Target>,
) -> Result<ResolvedSetOptionScope, RmuxError> {
    rmux_core::resolve_option_name(option)?;
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
        if is_user || supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Ok(ResolvedSetOptionScope::NoOp);
    }

    if pane {
        let Some(Target::Pane(target)) = target else {
            return Err(RmuxError::Server(
                "set-option -p requires a pane target".to_owned(),
            ));
        };
        let scope = OptionScopeSelector::Pane(target);
        if !is_user && !supports_scope(&scope) {
            return Err(RmuxError::Server(
                "pane scope is not supported for this option".to_owned(),
            ));
        }
        return Ok(scope.into());
    }

    if window {
        if global {
            let scope = OptionScopeSelector::WindowGlobal;
            if !is_user && !supports_scope(&scope) {
                return Err(RmuxError::Server(
                    "window scope is not supported for this option".to_owned(),
                ));
            }
            return Ok(scope.into());
        }

        let Some(target) = target else {
            return Err(RmuxError::Server(
                "set-window-option requires a window target or -g".to_owned(),
            ));
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
            return Err(RmuxError::Server(
                "window scope is not supported for this option".to_owned(),
            ));
        }
        return Ok(scope.into());
    }

    if global {
        let scope = rmux_core::default_global_scope_for_option_name(option)?;
        if !is_user && !supports_scope(&scope) {
            return Err(RmuxError::Server(
                "global scope is not supported for this option".to_owned(),
            ));
        }
        return Ok(scope.into());
    }

    let Some(target) = target else {
        if !(server && append) {
            return Err(RmuxError::Server(
                "set-option requires a target or one of -g, -s, -w, or -p".to_owned(),
            ));
        }
        let scope = rmux_core::default_global_scope_for_option_name(option)?;
        if !is_user && !supports_scope(&scope) {
            return Err(RmuxError::Server(
                "global scope is not supported for this option".to_owned(),
            ));
        }
        return Ok(scope.into());
    };

    let scope = match target {
        Target::Session(session_name) => OptionScopeSelector::Session(session_name),
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
        return Err(RmuxError::Server(
            "target scope is not supported for this option".to_owned(),
        ));
    }

    Ok(scope.into())
}

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

fn resolve_show_options_scope(
    command: &str,
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
    name: Option<&str>,
) -> Result<OptionScopeSelector, RmuxError> {
    if global && pane {
        return Err(RmuxError::Server(format!(
            "{command} does not support combining -g and -p"
        )));
    }

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
        (true, false, None) => Err(RmuxError::Server(format!("{command} -w requires a target"))),
        (false, true, Some(Target::Pane(target))) => Ok(OptionScopeSelector::Pane(target)),
        (false, true, Some(_)) => Err(RmuxError::Server(format!(
            "{command} -p requires a pane target"
        ))),
        (false, true, None) => Err(RmuxError::Server(format!("{command} -p requires a target"))),
        (false, false, _) if global => match name {
            Some(name) => rmux_core::default_global_scope_for_option_name(name),
            None => Ok(OptionScopeSelector::SessionGlobal),
        },
        (false, false, Some(Target::Session(session_name))) => {
            Ok(OptionScopeSelector::Session(session_name))
        }
        (false, false, Some(Target::Window(target))) => Ok(OptionScopeSelector::Window(target)),
        (false, false, Some(Target::Pane(target))) => Ok(OptionScopeSelector::Pane(target)),
        (false, false, None) => Err(RmuxError::Server(format!(
            "{command} requires -g, -s, or a target"
        ))),
        (true, true, _) => unreachable!("validated conflicting show-options scope flags"),
    }
}
