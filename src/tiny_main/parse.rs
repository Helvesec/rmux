use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use rmux_proto::request::{AttachSessionExt2Request, NewSessionExtRequest};
use rmux_proto::{
    CapturePaneTargetActionRequest, OptionScopeSelector, PaneTarget, ResizePaneAdjustment,
    ResizePaneRelativeDirection, ResizePaneTargetActionRequest, SessionName,
    SplitWindowTargetActionRequest, Target, TerminalSize, WindowTarget,
};

use crate::cli_args::schema;
use crate::client_terminal::client_terminal_context_from_parts;

mod admission;
use admission::{canonical, Tail};

mod types;
pub(super) use types::*;

pub(super) fn parse_has_session(args: &[OsString]) -> Option<TinyHasSession> {
    if args.iter().filter(|arg| *arg == "-t").count() != 1 {
        return None;
    }
    let parsed = canonical::<schema::HasSession>(args, "t", Tail::None)?;
    Some(TinyHasSession {
        target: parse_session_name(parsed.target.as_deref()?)?,
    })
}

pub(super) fn parse_list_windows(args: &[OsString]) -> Option<TinyListWindows> {
    if args.iter().filter(|arg| *arg == "-t").count() != 1 {
        return None;
    }
    let parsed = canonical::<schema::ListWindows>(args, "t", Tail::None)?;
    Some(TinyListWindows {
        target: parse_session_name(parsed.target?.raw())?,
    })
}

pub(super) fn parse_list_panes(args: &[OsString]) -> Option<TinyListPanes> {
    if args
        .iter()
        .filter(|arg| *arg == "-t" || *arg == "-a")
        .count()
        != 1
    {
        return None;
    }
    let parsed = canonical::<schema::ListPanes>(args, "ta", Tail::None)?;
    if parsed.all_sessions {
        Some(TinyListPanes::AllSessions)
    } else {
        parse_list_panes_target(parsed.target?.raw())
    }
}

pub(super) fn parse_capture_pane(args: &[OsString]) -> Option<CapturePaneTargetActionRequest> {
    let parsed = canonical::<schema::CapturePane>(args, "ptSE", Tail::None)?;
    if !parsed.print {
        return None;
    }
    let bound = |value: Option<String>| -> Option<Option<i64>> {
        match value {
            None => Some(None),
            Some(value) => value.parse::<i64>().ok().map(Some),
        }
    };
    Some(CapturePaneTargetActionRequest {
        target: parsed.target.map(|target| target.raw().to_owned()),
        start: bound(parsed.start)?,
        end: bound(parsed.end)?,
        print: parsed.print,
        buffer_name: parsed.buffer_name,
        alternate: parsed.alternate,
        escape_ansi: parsed.escape_ansi,
        escape_sequences: parsed.escape_sequences,
        include_format: parsed.include_format,
        hyperlinks: parsed.hyperlinks,
        line_numbers: parsed.line_numbers,
        join_wrapped: parsed.join_wrapped,
        use_mode_screen: parsed.use_mode_screen,
        preserve_trailing_spaces: parsed.preserve_trailing_spaces,
        do_not_trim_spaces: parsed.do_not_trim_spaces,
        pending_input: parsed.pending_input,
        quiet: parsed.quiet,
        start_is_absolute: false,
        end_is_absolute: false,
    })
}

pub(super) fn parse_split_window(args: &[OsString]) -> Option<SplitWindowTargetActionRequest> {
    let parsed = canonical::<schema::SplitWindow>(args, "hvdbZfktcl", Tail::Command)?;
    Some(SplitWindowTargetActionRequest {
        direction: parsed.direction(),
        size: parsed.size_spec(),
        target: parsed.target.map(|target| target.raw().to_owned()),
        before: parsed.before,
        environment: None,
        command: nonempty(parsed.command),
        process_command: None,
        start_directory: parsed.start_directory,
        keep_alive_on_exit: parsed.keep_alive_on_exit.then_some(true),
        detached: parsed.detached,
        preserve_zoom: parsed.preserve_zoom,
        full_size: parsed.full_size,
        stdin_payload: None,
    })
}

pub(super) fn parse_new_window(args: &[OsString]) -> Option<TinyNewWindow> {
    let parsed = canonical::<schema::NewWindow>(args, "dtnc", Tail::Command)?;
    Some(TinyNewWindow {
        target: parse_session_name(parsed.target?.raw())?,
        name: parsed.name,
        detached: parsed.detached,
        command: nonempty(parsed.command),
        start_directory: parsed.start_directory,
    })
}

pub(super) fn parse_kill_session(args: &[OsString]) -> Option<TinyKillSession> {
    let parsed = canonical::<schema::KillSession>(args, "t", Tail::None)?;
    Some(TinyKillSession {
        target: parse_session_name(parsed.target?.raw())?,
    })
}

pub(super) fn parse_show_options(args: &[OsString], force_window: bool) -> Option<TinyShowOptions> {
    let parsed = if force_window {
        canonical::<schema::ShowWindowOptions>(args, "g", Tail::None)?
    } else {
        canonical::<schema::ShowOptions>(args, "g", Tail::None)?
    };
    if !parsed.global {
        return None;
    }
    Some(TinyShowOptions {
        scope: if force_window {
            OptionScopeSelector::WindowGlobal
        } else {
            OptionScopeSelector::SessionGlobal
        },
    })
}

pub(super) fn parse_rename_window(args: &[OsString]) -> Option<TinyRenameWindow> {
    let parsed = canonical::<schema::RenameWindow>(args, "t", Tail::Single)?;
    Some(TinyRenameWindow {
        target: parse_window_target(parsed.target?.raw())?,
        name: parsed.new_name,
    })
}

pub(super) fn parse_select_window(args: &[OsString]) -> Option<TinySelectWindow> {
    let parsed = canonical::<schema::SelectWindow>(args, "t", Tail::None)?;
    Some(TinySelectWindow {
        target: parse_window_target(parsed.target?.raw())?,
    })
}

pub(super) fn parse_kill_pane(args: &[OsString]) -> Option<TinyKillPane> {
    let parsed = canonical::<schema::KillPane>(args, "t", Tail::None)?;
    Some(TinyKillPane {
        target: parse_pane_target(parsed.target?.raw())?,
    })
}

pub(super) fn parse_join_pane(args: &[OsString]) -> Option<TinyJoinPane> {
    let parsed = canonical::<schema::JoinPane>(args, "dst", Tail::None)?;
    Some(TinyJoinPane {
        source: parse_pane_target(parsed.source?.raw())?,
        target: parse_pane_target(parsed.target?.raw())?,
        detached: parsed.detached,
    })
}

pub(super) fn parse_set_option(args: &[OsString], force_window: bool) -> Option<TinySetOption> {
    let parsed = if force_window {
        canonical::<schema::SetWindowOption>(args, "g", Tail::Values)?
    } else {
        canonical::<schema::SetOption>(args, "g", Tail::Values)?
    };
    if !parsed.global {
        return None;
    }
    let scope = if force_window {
        OptionScopeSelector::WindowGlobal
    } else {
        rmux_core::default_global_scope_for_option_name(&parsed.option).ok()?
    };
    Some(TinySetOption {
        scope,
        option: parsed.option,
        value: parsed.value?,
    })
}

pub(super) fn parse_attach_session(args: &[OsString]) -> Option<AttachSessionExt2Request> {
    let parsed = canonical::<schema::AttachSession>(args, "tdxrEc", Tail::None)?;
    Some(AttachSessionExt2Request {
        target: None,
        target_spec: parsed.target.map(|target| target.raw().to_owned()),
        detach_other_clients: parsed.detach_other_clients || parsed.kill_other_clients,
        kill_other_clients: parsed.kill_other_clients,
        read_only: parsed.read_only,
        skip_environment_update: parsed.skip_environment_update,
        flags: None,
        working_directory: parsed.working_directory,
        client_terminal: client_terminal_context_from_parts(
            Vec::new(),
            infer_client_utf8_from_env(),
        ),
        client_size: rmux_os::terminal::current_size(),
    })
}

pub(super) fn parse_new_session(args: &[OsString]) -> Option<NewSessionExtRequest> {
    let parsed = canonical::<schema::NewSession>(args, "dEsncxy", Tail::Command)?;
    if !parsed.detached || parsed.cols == Some(0) || parsed.rows == Some(0) {
        return None;
    }
    Some(NewSessionExtRequest {
        session_name: parsed.session_name,
        working_directory: parsed
            .working_directory
            .or_else(current_working_directory_string),
        detached: parsed.detached,
        size: build_terminal_size(parsed.cols, parsed.rows),
        environment: None,
        group_target: parsed.group_target,
        attach_if_exists: parsed.attach_if_exists,
        detach_other_clients: parsed.detach_other_clients,
        kill_other_clients: parsed.kill_other_clients,
        flags: None,
        window_name: parsed.window_name,
        print_session_info: parsed.print_session_info,
        print_format: parsed.print_format,
        command: nonempty(parsed.command),
        process_command: None,
        client_environment: invoking_client_environment(),
        skip_environment_update: parsed.skip_environment_update,
    })
}

pub(super) fn parse_display_message(args: &[OsString]) -> Option<TinyDisplayMessage> {
    let parsed = canonical::<schema::DisplayMessage>(args, "ptF", Tail::Single)?;
    if !parsed.print || (parsed.format.is_some() && !parsed.message.is_empty()) {
        return None;
    }
    let target = match parsed.target {
        Some(target) if target.contains(':') => return None,
        Some(target) => Some(parse_target(&target)?),
        None => None,
    };
    Some(TinyDisplayMessage {
        target,
        message: parsed.format.or_else(|| parsed.message.into_iter().next()),
    })
}

pub(super) fn parse_send_keys(args: &[OsString]) -> Option<TinySendKeys> {
    let parsed = canonical::<schema::SendKeys>(args, "t", Tail::Values)?;
    Some(TinySendKeys {
        raw_target: parsed.target?.raw().to_owned(),
        keys: parsed.keys,
    })
}

pub(super) fn parse_source_file(args: &[OsString]) -> Option<TinySourceFile> {
    if nested_multiplexer_context() {
        return None;
    }
    let parsed = canonical::<schema::SourceFile>(args, "", Tail::Command)?;
    let paths = parsed.paths;
    if paths.is_empty() || paths.iter().any(|path| path == "-") {
        return None;
    }
    Some(TinySourceFile { paths })
}

pub(super) fn parse_resize_pane(args: &[OsString]) -> Option<ResizePaneTargetActionRequest> {
    let normalized = args
        .iter()
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    let normalized =
        rmux_core::tmux_precedence::normalize_tmux_precedence("resize-pane", normalized)
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
    let parsed = canonical::<schema::ResizePane>(&normalized, "txyLRUDZ", Tail::Resize)?;
    let cells = |size| match size {
        None => Some(None),
        Some(crate::cli_args::ResizePaneSize::Cells(value)) => Some(Some(value)),
        _ => None,
    };
    let relative = parsed
        .left
        .map(|n| (ResizePaneRelativeDirection::Left, n))
        .or_else(|| {
            parsed
                .right
                .map(|n| (ResizePaneRelativeDirection::Right, n))
        })
        .or_else(|| parsed.up.map(|n| (ResizePaneRelativeDirection::Up, n)))
        .or_else(|| parsed.down.map(|n| (ResizePaneRelativeDirection::Down, n)));
    let columns = cells(parsed.columns)?;
    let rows = cells(parsed.rows)?;
    Some(ResizePaneTargetActionRequest {
        target: parsed.target.map(|target| target.raw().to_owned()),
        adjustment: if parsed.zoom {
            ResizePaneAdjustment::Zoom
        } else {
            resize_adjustment(columns, rows, relative)
        },
    })
}

fn nonempty(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

pub(super) fn has_queue_separator(args: &[OsString]) -> bool {
    args.iter()
        .any(|arg| arg.to_str().is_some_and(argument_has_queue_terminator))
}

fn argument_has_queue_terminator(argument: &str) -> bool {
    argument
        .strip_suffix(';')
        .is_some_and(|stripped| !stripped.ends_with('\\'))
}

fn parse_target(value: &str) -> Option<Target> {
    if tmux_selector_target(value) {
        return None;
    }
    Target::parse(value).ok()
}

fn parse_pane_target(value: &str) -> Option<PaneTarget> {
    if tmux_selector_target(value) {
        return None;
    }
    match Target::parse(value).ok()? {
        Target::Pane(target) => Some(target),
        Target::Session(_) | Target::Window(_) => None,
    }
}

fn parse_list_panes_target(value: &str) -> Option<TinyListPanes> {
    if tmux_selector_target(value) {
        return None;
    }
    match Target::parse(value).ok()? {
        Target::Session(target) => Some(TinyListPanes::Target {
            target,
            target_window_index: None,
        }),
        Target::Window(target) => Some(TinyListPanes::Target {
            target: target.session_name().clone(),
            target_window_index: Some(target.window_index()),
        }),
        Target::Pane(target) => Some(TinyListPanes::Target {
            target: target.session_name().clone(),
            target_window_index: Some(target.window_index()),
        }),
    }
}

fn parse_session_name(value: &str) -> Option<SessionName> {
    if tmux_selector_target(value) {
        return None;
    }
    match Target::parse(value).ok()? {
        Target::Session(target) => Some(target),
        Target::Window(_) | Target::Pane(_) => None,
    }
}

fn parse_window_target(value: &str) -> Option<WindowTarget> {
    if tmux_selector_target(value) {
        return None;
    }
    match Target::parse(value).ok()? {
        Target::Window(target) => Some(target),
        Target::Session(_) | Target::Pane(_) => None,
    }
}

fn tmux_selector_target(value: &str) -> bool {
    matches!(
        value.as_bytes().first(),
        Some(b'%' | b'@' | b'!' | b'=' | b'~' | b'{' | b'$' | b'+' | b'-')
    )
}

fn resize_adjustment(
    columns: Option<u16>,
    rows: Option<u16>,
    relative: Option<(ResizePaneRelativeDirection, u16)>,
) -> ResizePaneAdjustment {
    match (columns, rows, relative) {
        (Some(columns), Some(rows), Some((relative, cells))) => ResizePaneAdjustment::Composite {
            columns: Some(columns),
            rows: Some(rows),
            relative: Some(relative),
            cells,
        },
        (Some(columns), None, Some((relative, cells))) => ResizePaneAdjustment::Composite {
            columns: Some(columns),
            rows: None,
            relative: Some(relative),
            cells,
        },
        (None, Some(rows), Some((relative, cells))) => ResizePaneAdjustment::Composite {
            columns: None,
            rows: Some(rows),
            relative: Some(relative),
            cells,
        },
        (Some(columns), Some(rows), None) => ResizePaneAdjustment::AbsoluteSize { columns, rows },
        (Some(columns), None, None) => ResizePaneAdjustment::AbsoluteWidth { columns },
        (None, Some(rows), None) => ResizePaneAdjustment::AbsoluteHeight { rows },
        (None, None, Some((relative, cells))) => relative.to_adjustment(cells),
        (None, None, None) => ResizePaneAdjustment::NoOp,
    }
}

fn build_terminal_size(cols: Option<u16>, rows: Option<u16>) -> Option<TerminalSize> {
    match (cols, rows) {
        (None, None) => None,
        (cols, rows) => Some(TerminalSize {
            cols: cols.unwrap_or(80),
            rows: rows.unwrap_or(24),
        }),
    }
}

fn current_working_directory_string() -> Option<String> {
    env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

pub(super) fn nested_multiplexer_context() -> bool {
    env::var_os("RMUX").is_some_and(|value| !value.is_empty())
        || env::var_os("TMUX").is_some_and(|value| !value.is_empty())
}

fn infer_client_utf8_from_env() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG", "RMUX"]
        .into_iter()
        .filter_map(env::var_os)
        .any(|value| {
            let value = value.to_string_lossy().to_ascii_uppercase();
            value.contains("UTF-8") || value.contains("UTF8")
        })
}

#[cfg(windows)]
const RMUX_CLIENT_SHELL_ENV: &str = "RMUX_CLIENT_SHELL";
#[cfg(windows)]
const INTERNAL_CLIENT_SHELL_ENV: &str = "RMUX_INTERNAL_CLIENT_SHELL";
#[cfg(windows)]
const INTERNAL_TMUX_COMPAT_ENV: &str = "RMUX_INTERNAL_INVOKED_AS_TMUX";

#[cfg(windows)]
fn invoking_client_environment() -> Option<Vec<String>> {
    Some(windows_invoking_client_environment(
        env::vars_os(),
        invoking_client_shell(),
    ))
}

#[cfg(windows)]
pub(super) fn windows_invoking_client_environment<I>(vars: I, shell: Option<String>) -> Vec<String>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut environment = vars
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .filter(|(name, _)| !name.starts_with('='))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(RMUX_CLIENT_SHELL_ENV))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(INTERNAL_CLIENT_SHELL_ENV))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(INTERNAL_TMUX_COMPAT_ENV))
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();

    if let Some(shell) = shell {
        environment.push(format!("{RMUX_CLIENT_SHELL_ENV}={shell}"));
    }

    environment
}

#[cfg(windows)]
pub(super) fn invoking_client_shell() -> Option<String> {
    let parent_pid = rmux_os::process::parent_pid(std::process::id())?;
    let parent_name = rmux_os::process::command_name(parent_pid)?;
    let environment = crate::windows_shell::WindowsShellEnvironment::current();
    windows_client_shell_for_parent_name(&parent_name, &environment)
}

#[cfg(windows)]
pub(super) fn windows_client_shell_for_parent_name(
    parent_name: &str,
    environment: &crate::windows_shell::WindowsShellEnvironment,
) -> Option<String> {
    crate::windows_shell::client_shell_for_parent_name(parent_name, environment)
}

#[cfg(not(windows))]
fn invoking_client_environment() -> Option<Vec<String>> {
    None
}
