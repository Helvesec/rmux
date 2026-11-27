use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::request::Request;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    PaneTarget, RmuxError, SessionName, SetOptionByNameRequest, SetOptionMode, Target,
};

use super::parse_target_arg;
use super::targets::implicit_pane_target;
use super::tokens::CommandTokens;
use super::values::{reject_unknown_option_before_positional, unsupported_flag};

mod environment;
mod hooks;
mod set_option_scope;
mod show_options;

pub(super) use environment::{parse_set_environment, parse_show_environment};
pub(super) use hooks::{parse_set_hook, parse_show_hooks};
use set_option_scope::{resolve_set_option_scope, SetOptionScopeContext};
pub(super) use show_options::parse_show_options;

pub(super) enum ParsedSetOptionCommand {
    Request(Box<Request>),
    QuietOnlyIfUnset(Box<Request>),
    Ignored(String),
    NoOp,
}

pub(super) fn parse_set_option(
    args: CommandTokens,
    force_window: bool,
    default_target: Option<Target>,
    sessions: &ManagedSessionStore,
) -> Result<Request, RmuxError> {
    match parse_set_option_invocation(args, force_window, default_target, sessions)? {
        ParsedSetOptionCommand::Request(request)
        | ParsedSetOptionCommand::QuietOnlyIfUnset(request) => Ok(*request),
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
    sessions: &ManagedSessionStore,
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
            _ => {
                reject_unknown_option_before_positional(command_name, token)?;
                break;
            }
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
        SetOptionScopeContext {
            target: scope_target,
            sessions,
        },
    )?;
    let Some(scope) = scope.into_scope() else {
        return Ok(ParsedSetOptionCommand::NoOp);
    };
    let mode = if flags.append {
        SetOptionMode::Append
    } else {
        SetOptionMode::Replace
    };
    // The value is validated when the request executes, as tmux does in
    // cmd_set_option_exec: an invalid value fails that command only and the
    // rest of a sourced file still runs.

    let wrap = if flags.quiet && flags.only_if_unset {
        ParsedSetOptionCommand::QuietOnlyIfUnset
    } else {
        ParsedSetOptionCommand::Request
    };
    Ok(wrap(Box::new(Request::SetOptionByName(Box::new(
        SetOptionByNameRequest {
            scope,
            name: option,
            value,
            mode,
            only_if_unset: flags.only_if_unset,
            unset: flags.unset,
            unset_pane_overrides: flags.unset_pane_overrides,
            format: flags.format,
            format_target: flags.format.then_some(format_target).flatten(),
        },
    )))))
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

pub(super) fn default_set_option_target(
    sessions: &ManagedSessionStore,
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

fn is_user_option_name(option: &str) -> bool {
    option
        .split('[')
        .next()
        .is_some_and(|base| base.starts_with('@'))
}

fn dummy_pane_scope() -> OptionScopeSelector {
    OptionScopeSelector::Pane(PaneTarget::with_window(
        SessionName::new("set-option").expect("valid session name"),
        0,
        0,
    ))
}
