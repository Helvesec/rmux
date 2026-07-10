use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::request::Request;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    OptionName, PaneTarget, RmuxError, ScopeSelector, SessionName, SetEnvironmentMode,
    SetEnvironmentRequest, SetOptionByNameRequest, SetOptionMode, ShowEnvironmentRequest,
    ShowOptionsRequest, Target, WindowTarget,
};

use super::targets::{implicit_pane_target, implicit_session_name, implicit_window_target};
use super::tokens::CommandTokens;
use super::values::unsupported_flag;
use super::{parse_session_name, parse_target_arg};

#[path = "config_parse/hooks.rs"]
mod hooks;

pub(super) use hooks::{parse_set_hook, parse_show_hooks};

pub(super) enum ParsedSetOptionCommand {
    Request(Box<Request>),
    Ignored(String),
    NoOp,
}

pub(super) fn parse_set_option(
    args: CommandTokens,
    force_window: bool,
    default_target: Option<Target>,
) -> Result<Request, RmuxError> {
    match parse_set_option_invocation(args, force_window, default_target)? {
        ParsedSetOptionCommand::Request(request) => Ok(*request),
        ParsedSetOptionCommand::Ignored(message) => Err(RmuxError::Server(message)),
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
            "-s" if !force_window => {
                let _ = args.optional();
                flags.select_server();
            }
            "-w" if !force_window => {
                let _ = args.optional();
                flags.select_window();
            }
            "-p" if !force_window => {
                let _ = args.optional();
                flags.select_pane();
            }
            "-q" => {
                let _ = args.optional();
                flags.quiet = true;
            }
            "-s" | "-w" | "-p" | "-U" if force_window => {
                return Err(unsupported_flag(command_name, token));
            }
            "-a" => {
                let _ = args.optional();
                flags.append = true;
            }
            "-F" => {
                let _ = args.optional();
                flags.format = true;
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
                // tmux -U acts like -u at whatever scope -p/-w/-g select
                // (session by default); the pane-override sweep applies only
                // when the resolved scope is a window.
                flags.unset_pane_overrides = true;
                flags.unset = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_target_arg("set-option", args.required("-t target")?)?);
            }
            token if is_set_option_flag_cluster(token) => {
                let token = args
                    .optional()
                    .expect("peeked set-option flag cluster must be present");
                flags.apply_cluster(command_name, &token, force_window)?;
            }
            _ => break,
        }
    }

    let option = args.required("set-option option")?;
    let value = args.optional();
    args.no_extra("set-option")?;

    if let Err(error) = rmux_core::resolve_option_name_typed(&option) {
        if flags.quiet && error.is_quiet_set_option_lookup_error() {
            return Ok(ParsedSetOptionCommand::Ignored(
                error.into_rmux_error().to_string(),
            ));
        }
        return Err(error.into_rmux_error());
    }

    let format_target = target.clone().or(default_target.clone());
    let scope_target = set_option_scope_target(&option, &flags, target, default_target)?;
    let scope = resolve_set_option_scope(
        &option,
        flags.global,
        flags.server,
        flags.window,
        flags.pane,
        flags.append,
        scope_target,
    )?;
    let Some(scope) = scope.into_scope() else {
        return Ok(ParsedSetOptionCommand::NoOp);
    };
    let mode = if flags.append {
        SetOptionMode::Append
    } else {
        SetOptionMode::Replace
    };
    if !flags.format && !should_defer_set_option_value_validation(&option, value.as_deref()) {
        rmux_core::validate_option_name_mutation(
            &option,
            &scope,
            mode,
            value.as_deref(),
            flags.unset,
        )?;
    }

    Ok(ParsedSetOptionCommand::Request(Box::new(
        Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope,
            name: option,
            value,
            mode,
            only_if_unset: flags.only_if_unset,
            unset: flags.unset,
            unset_pane_overrides: flags.unset_pane_overrides,
            format: flags.format,
            format_target: flags.format.then_some(format_target).flatten(),
        })),
    )))
}

fn set_option_scope_target(
    option: &str,
    flags: &SetOptionFlags,
    explicit_target: Option<Target>,
    default_target: Option<Target>,
) -> Result<Option<Target>, RmuxError> {
    if explicit_target.is_some() {
        return Ok(explicit_target);
    }
    if flags.pane || flags.window {
        return Ok(default_target);
    }
    if flags.global {
        return Ok(None);
    }
    if flags.server {
        if !is_user_option_name(option)
            && matches!(
                rmux_core::default_global_scope_for_option_name(option)?,
                OptionScopeSelector::ServerGlobal
            )
        {
            return Ok(None);
        }
        return Ok(default_target);
    }
    if is_user_option_name(option) {
        return Ok(default_target);
    }
    let default_scope = rmux_core::default_global_scope_for_option_name(option)?;
    if matches!(default_scope, OptionScopeSelector::ServerGlobal) {
        return Ok(None);
    }
    Ok(default_target)
}

fn should_defer_set_option_value_validation(option: &str, value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if !value.contains("#{") {
        return false;
    }
    rmux_core::option_name_by_name(option) == Some(OptionName::ExtendedKeys)
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
    format: bool,
    only_if_unset: bool,
    unset: bool,
    unset_pane_overrides: bool,
    quiet: bool,
}

impl SetOptionFlags {
    fn new(force_window: bool) -> Self {
        Self {
            global: false,
            server: false,
            window: force_window,
            pane: false,
            append: false,
            format: false,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            quiet: false,
        }
    }

    fn apply_cluster(
        &mut self,
        command_name: &str,
        token: &str,
        force_window: bool,
    ) -> Result<(), RmuxError> {
        for flag in token[1..].chars() {
            if force_window && matches!(flag, 's' | 'w' | 'p' | 'U') {
                return Err(unsupported_flag(command_name, &format!("-{flag}")));
            }
            match flag {
                'g' => self.global = true,
                's' => self.select_server(),
                'w' => self.select_window(),
                'p' => self.select_pane(),
                'q' => self.quiet = true,
                'a' => self.append = true,
                'F' => self.format = true,
                'o' => self.only_if_unset = true,
                'u' => self.unset = true,
                // -U is an unset modifier, not a scope selector: plain
                // `set -U` targets the session copy (oracle 2026-07-09) and
                // -p/-w keep their own precedence when combined with it, so
                // clusters such as -qU or -gU must not coerce window scope.
                'U' => {
                    self.unset_pane_overrides = true;
                    self.unset = true;
                }
                _ => return Err(unsupported_flag(command_name, &format!("-{flag}"))),
            }
        }
        Ok(())
    }

    fn select_server(&mut self) {
        self.server = true;
    }

    fn select_window(&mut self) {
        self.window = true;
    }

    fn select_pane(&mut self) {
        self.pane = true;
    }
}

fn is_set_option_flag_cluster(token: &str) -> bool {
    token.starts_with('-')
        && !token.starts_with("--")
        && token.len() > 2
        && token[1..].chars().all(|flag| {
            matches!(
                flag,
                'g' | 'a' | 'F' | 'o' | 'q' | 'u' | 's' | 'w' | 'p' | 'U'
            )
        })
}

pub(super) fn parse_set_environment(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
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

    let scope =
        build_global_or_session_scope("set-environment", global, target, sessions, find_context)?;
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

    Ok(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
        scope,
        name,
        value,
        mode,
        hidden,
        format,
    })))
}

pub(super) fn parse_show_options(
    mut args: CommandTokens,
    force_window: bool,
    sessions: &SessionStore,
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
            _ => break,
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

pub(super) fn parse_show_environment(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
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

    let scope =
        build_global_or_session_scope("show-environment", global, target, sessions, find_context)?;
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
            .all(|flag| matches!(flag, 'A' | 'H' | 'g' | 's' | 'w' | 'p' | 'v' | 'q'))
}

fn build_global_or_session_scope(
    command: &str,
    global: bool,
    target: Option<SessionName>,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<ScopeSelector, RmuxError> {
    match (global, target) {
        (true, None) => Ok(ScopeSelector::Global),
        (false, Some(session_name)) => Ok(ScopeSelector::Session(session_name)),
        (false, None) => Ok(ScopeSelector::Session(implicit_session_name(
            sessions,
            find_context,
            command,
        )?)),
        _ => Err(RmuxError::Server(format!(
            "{command} accepts at most one of -g or -t target"
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
    let is_user = is_user_option_name(option);
    let supports_scope = |scope: &OptionScopeSelector| option_name_supports_scope(option, scope);

    if pane && !server && !window {
        match target.clone() {
            Some(Target::Pane(target)) => {
                let scope = OptionScopeSelector::Pane(target);
                if is_user || supports_scope(&scope) {
                    return Ok(scope.into());
                }
            }
            None if is_user || option_supports_pane_scope(option) => {
                return Ok(ResolvedSetOptionScope::NoOp);
            }
            _ => {}
        }
    }

    if global && !is_user && (server || pane || window) {
        let scope = rmux_core::default_global_scope_for_option_name(option)?;
        if supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Err(RmuxError::Server(
            "global scope is not supported for this option".to_owned(),
        ));
    }

    if !global && !is_user && (server || pane || window) {
        let scope = natural_known_set_option_scope(option, target)?;
        return Ok(scope.into());
    }

    if server {
        let scope = OptionScopeSelector::ServerGlobal;
        if is_user || supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Ok(ResolvedSetOptionScope::NoOp);
    }

    if global && pane {
        return Ok(OptionScopeSelector::WindowGlobal.into());
    }

    if pane {
        let Some(Target::Pane(target)) = target else {
            return Err(RmuxError::Server(
                "set-option -p requires a pane target".to_owned(),
            ));
        };
        let scope = OptionScopeSelector::Pane(target);
        return Ok(scope.into());
    }

    if window {
        if global {
            let scope = OptionScopeSelector::WindowGlobal;
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
        let default_scope = rmux_core::default_global_scope_for_option_name(option)?;
        if matches!(default_scope, OptionScopeSelector::ServerGlobal) {
            if !is_user && !supports_scope(&default_scope) {
                return Err(RmuxError::Server(
                    "global scope is not supported for this option".to_owned(),
                ));
            }
            return Ok(default_scope.into());
        }
        if !(server && append) {
            return Err(RmuxError::Server(
                "set-option requires a target or one of -g, -s, -w, or -p".to_owned(),
            ));
        }
        if !is_user && !supports_scope(&default_scope) {
            return Err(RmuxError::Server(
                "global scope is not supported for this option".to_owned(),
            ));
        }
        return Ok(default_scope.into());
    };

    let scope = match target.clone() {
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
        // Fall back to the option's natural table scope (mirroring the
        // explicit-flag path, which trusts natural_known_set_option_scope):
        // e.g. a flagless `set -U -t alpha:0 pane-border-style` resolves at
        // window scope like tmux instead of dead-ending at session scope.
        return Ok(natural_known_set_option_scope(option, Some(target))?.into());
    }

    Ok(scope.into())
}

fn is_user_option_name(option: &str) -> bool {
    option
        .split('[')
        .next()
        .is_some_and(|base| base.starts_with('@'))
}

fn natural_known_set_option_scope(
    option: &str,
    target: Option<Target>,
) -> Result<OptionScopeSelector, RmuxError> {
    match rmux_core::default_global_scope_for_option_name(option)? {
        OptionScopeSelector::ServerGlobal => Ok(OptionScopeSelector::ServerGlobal),
        OptionScopeSelector::SessionGlobal => {
            let Some(target) = target else {
                return Err(RmuxError::Server(
                    "set-option requires a target or one of -g, -s, -w, or -p".to_owned(),
                ));
            };
            Ok(OptionScopeSelector::Session(match target {
                Target::Session(session_name) => session_name,
                Target::Window(target) => target.session_name().clone(),
                Target::Pane(target) => target.session_name().clone(),
            }))
        }
        OptionScopeSelector::WindowGlobal => {
            let Some(target) = target else {
                return Err(RmuxError::Server(
                    "set-window-option requires a window target or -g".to_owned(),
                ));
            };
            Ok(OptionScopeSelector::Window(match target {
                Target::Session(session_name) => WindowTarget::new(session_name),
                Target::Window(target) => target,
                Target::Pane(target) => {
                    WindowTarget::with_window(target.session_name().clone(), target.window_index())
                }
            }))
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

struct ShowOptionsScopeRequest<'a> {
    command_name: &'a str,
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    target: Option<Target>,
    name: Option<&'a str>,
    quiet: bool,
    sessions: &'a SessionStore,
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
    sessions: &SessionStore,
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
    sessions: &SessionStore,
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
    sessions: &SessionStore,
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
    sessions: &SessionStore,
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
