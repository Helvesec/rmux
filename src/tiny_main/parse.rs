use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use rmux_proto::request::{AttachSessionExt2Request, NewSessionExtRequest};
use rmux_proto::{
    CapturePaneTargetActionRequest, OptionScopeSelector, PaneTarget, ResizePaneAdjustment,
    ResizePaneRelativeDirection, ResizePaneTargetActionRequest, SessionName, SplitDirection,
    SplitWindowTargetActionRequest, Target, TerminalSize, WindowTarget,
};

use crate::client_terminal::client_terminal_context_from_parts;

pub(super) struct TinyDisplayMessage {
    pub(super) target: Option<Target>,
    pub(super) message: Option<String>,
}

pub(super) struct TinySendKeys {
    pub(super) target: PaneTarget,
    pub(super) keys: Vec<String>,
}

pub(super) struct TinySourceFile {
    pub(super) paths: Vec<String>,
}

pub(super) struct TinyHasSession {
    pub(super) target: SessionName,
}

pub(super) struct TinyListWindows {
    pub(super) target: SessionName,
}

pub(super) enum TinyListPanes {
    AllSessions,
    Target {
        target: SessionName,
        target_window_index: Option<u32>,
    },
}

pub(super) struct TinyNewWindow {
    pub(super) target: SessionName,
    pub(super) name: Option<String>,
    pub(super) detached: bool,
    pub(super) command: Option<Vec<String>>,
    pub(super) start_directory: Option<PathBuf>,
}

pub(super) struct TinyKillSession {
    pub(super) target: SessionName,
}

pub(super) struct TinyShowOptions {
    pub(super) scope: OptionScopeSelector,
}

pub(super) struct TinyRenameWindow {
    pub(super) target: WindowTarget,
    pub(super) name: String,
}

pub(super) struct TinySelectWindow {
    pub(super) target: WindowTarget,
}

pub(super) struct TinyKillPane {
    pub(super) target: PaneTarget,
}

pub(super) struct TinyJoinPane {
    pub(super) source: PaneTarget,
    pub(super) target: PaneTarget,
    pub(super) detached: bool,
}

pub(super) struct TinySetOption {
    pub(super) scope: OptionScopeSelector,
    pub(super) option: String,
    pub(super) value: String,
}

pub(super) fn parse_has_session(args: &[OsString]) -> Option<TinyHasSession> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                if target.is_some() {
                    return None;
                }
                index += 1;
                target = Some(TinyHasSession {
                    target: parse_session_name(args.get(index)?.to_str()?)?,
                });
            }
            _ => return None,
        }
        index += 1;
    }

    target
}

pub(super) fn parse_list_windows(args: &[OsString]) -> Option<TinyListWindows> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                if target.is_some() {
                    return None;
                }
                index += 1;
                target = Some(parse_session_name(args.get(index)?.to_str()?)?);
            }
            "-a" | "-F" | "-f" | "--json" => return None,
            _ => return None,
        }
        index += 1;
    }

    Some(TinyListWindows { target: target? })
}

pub(super) fn parse_list_panes(args: &[OsString]) -> Option<TinyListPanes> {
    if has_queue_separator(args) {
        return None;
    }

    let mut all_sessions = false;
    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-a" => {
                if all_sessions || target.is_some() {
                    return None;
                }
                all_sessions = true;
            }
            "-t" => {
                if all_sessions || target.is_some() {
                    return None;
                }
                index += 1;
                target = Some(parse_list_panes_target(args.get(index)?.to_str()?)?);
            }
            "-s" | "-F" | "-f" | "--json" => return None,
            _ => return None,
        }
        index += 1;
    }

    if all_sessions {
        return Some(TinyListPanes::AllSessions);
    }
    target
}

pub(super) fn parse_capture_pane(args: &[OsString]) -> Option<CapturePaneTargetActionRequest> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut start = None;
    let mut end = None;
    let mut print = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-p" => print = true,
            "-t" => {
                index += 1;
                target = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-S" => {
                index += 1;
                let (value, absolute) = parse_capture_bound(args.get(index)?.to_str()?)?;
                if absolute {
                    return None;
                }
                start = value;
            }
            "-E" => {
                index += 1;
                let (value, absolute) = parse_capture_bound(args.get(index)?.to_str()?)?;
                if absolute {
                    return None;
                }
                end = value;
            }
            _ => return None,
        }
        index += 1;
    }
    if !print {
        return None;
    }

    Some(CapturePaneTargetActionRequest {
        target,
        start,
        end,
        print,
        buffer_name: None,
        alternate: false,
        escape_ansi: false,
        escape_sequences: false,
        join_wrapped: false,
        use_mode_screen: false,
        preserve_trailing_spaces: false,
        do_not_trim_spaces: false,
        pending_input: false,
        quiet: false,
        start_is_absolute: false,
        end_is_absolute: false,
    })
}

pub(super) fn parse_split_window(args: &[OsString]) -> Option<SplitWindowTargetActionRequest> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut direction = SplitDirection::Vertical;
    let mut detached = false;
    let mut before = false;
    let mut start_directory = None;
    let mut size = None;
    let mut preserve_zoom = false;
    let mut full_size = false;
    let mut command = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "--" => {
                index += 1;
                command = parse_command_tail(&args[index..])?;
                break;
            }
            "-h" => direction = SplitDirection::Horizontal,
            "-v" => direction = SplitDirection::Vertical,
            "-d" => detached = true,
            "-b" => before = true,
            "-Z" => preserve_zoom = true,
            "-f" => full_size = true,
            "-t" => {
                index += 1;
                target = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-c" => {
                index += 1;
                start_directory = Some(PathBuf::from(args.get(index)?));
            }
            "-l" => {
                index += 1;
                size = Some(args.get(index)?.to_str()?.to_owned());
            }
            value if value.starts_with('-') => return None,
            _ => {
                command = parse_command_tail(&args[index..])?;
                break;
            }
        }
        index += 1;
    }

    Some(SplitWindowTargetActionRequest {
        target,
        direction,
        before,
        environment: None,
        command,
        process_command: None,
        start_directory,
        keep_alive_on_exit: None,
        detached,
        size,
        preserve_zoom,
        full_size,
        stdin_payload: None,
    })
}

pub(super) fn parse_new_window(args: &[OsString]) -> Option<TinyNewWindow> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut name = None;
    let mut detached = false;
    let mut start_directory = None;
    let mut command = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "--" => {
                index += 1;
                command = parse_command_tail(&args[index..])?;
                break;
            }
            "-d" => detached = true,
            "-t" => {
                index += 1;
                target = Some(parse_session_name(args.get(index)?.to_str()?)?);
            }
            "-n" => {
                index += 1;
                name = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-c" => {
                index += 1;
                start_directory = Some(PathBuf::from(args.get(index)?));
            }
            "-a" | "-b" | "-e" | "-F" | "-k" | "-P" | "-S" => return None,
            value if value.starts_with('-') => return None,
            _ => {
                command = parse_command_tail(&args[index..])?;
                break;
            }
        }
        index += 1;
    }

    Some(TinyNewWindow {
        target: target?,
        name,
        detached,
        command,
        start_directory,
    })
}

pub(super) fn parse_kill_session(args: &[OsString]) -> Option<TinyKillSession> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                index += 1;
                target = Some(parse_session_name(args.get(index)?.to_str()?)?);
            }
            "-a" | "-C" => return None,
            _ => return None,
        }
        index += 1;
    }

    Some(TinyKillSession { target: target? })
}

pub(super) fn parse_show_options(args: &[OsString], force_window: bool) -> Option<TinyShowOptions> {
    if has_queue_separator(args) {
        return None;
    }

    let mut global = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-g" => global = true,
            _ => return None,
        }
        index += 1;
    }

    if !global {
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
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut name = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                index += 1;
                target = Some(parse_window_target(args.get(index)?.to_str()?)?);
            }
            value if value.starts_with('-') => return None,
            _ => {
                if name.is_some() || index + 1 != args.len() {
                    return None;
                }
                name = Some(arg.to_owned());
            }
        }
        index += 1;
    }

    Some(TinyRenameWindow {
        target: target?,
        name: name?,
    })
}

pub(super) fn parse_select_window(args: &[OsString]) -> Option<TinySelectWindow> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                index += 1;
                target = Some(parse_window_target(args.get(index)?.to_str()?)?);
            }
            _ => return None,
        }
        index += 1;
    }

    Some(TinySelectWindow { target: target? })
}

pub(super) fn parse_kill_pane(args: &[OsString]) -> Option<TinyKillPane> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                index += 1;
                target = Some(parse_pane_target(args.get(index)?.to_str()?)?);
            }
            "-a" => return None,
            _ => return None,
        }
        index += 1;
    }

    Some(TinyKillPane { target: target? })
}

pub(super) fn parse_join_pane(args: &[OsString]) -> Option<TinyJoinPane> {
    if has_queue_separator(args) {
        return None;
    }

    let mut source = None;
    let mut target = None;
    let mut detached = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-d" => detached = true,
            "-s" => {
                index += 1;
                source = Some(parse_pane_target(args.get(index)?.to_str()?)?);
            }
            "-t" => {
                index += 1;
                target = Some(parse_pane_target(args.get(index)?.to_str()?)?);
            }
            "-b" | "-f" | "-h" | "-l" | "-p" | "-v" => return None,
            _ => return None,
        }
        index += 1;
    }

    Some(TinyJoinPane {
        source: source?,
        target: target?,
        detached,
    })
}

pub(super) fn parse_set_option(args: &[OsString], force_window: bool) -> Option<TinySetOption> {
    if has_queue_separator(args) {
        return None;
    }

    let mut global = false;
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-g" => global = true,
            "-a" | "-F" | "-o" | "-q" | "-s" | "-u" | "-U" | "-w" | "-p" | "-t" => return None,
            value if value.starts_with('-') => return None,
            _ => values.push(arg.to_owned()),
        }
        index += 1;
    }

    if !global || values.len() != 2 {
        return None;
    }

    let option = values.remove(0);
    let value = values.remove(0);
    let scope = if force_window {
        OptionScopeSelector::WindowGlobal
    } else {
        rmux_core::default_global_scope_for_option_name(&option).ok()?
    };

    Some(TinySetOption {
        scope,
        option,
        value,
    })
}

pub(super) fn parse_attach_session(args: &[OsString]) -> Option<AttachSessionExt2Request> {
    if nested_multiplexer_context() || has_queue_separator(args) {
        return None;
    }

    let mut target_spec = None;
    let mut detach_other_clients = false;
    let mut kill_other_clients = false;
    let mut read_only = false;
    let mut skip_environment_update = false;
    let mut working_directory = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                index += 1;
                target_spec = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-d" => detach_other_clients = true,
            "-x" => {
                detach_other_clients = true;
                kill_other_clients = true;
            }
            "-r" => read_only = true,
            "-E" => skip_environment_update = true,
            "-c" => {
                index += 1;
                working_directory = Some(args.get(index)?.to_str()?.to_owned());
            }
            _ => return None,
        }
        index += 1;
    }

    Some(AttachSessionExt2Request {
        target: None,
        target_spec,
        detach_other_clients,
        kill_other_clients,
        read_only,
        skip_environment_update,
        flags: None,
        working_directory,
        client_terminal: client_terminal_context_from_parts(
            Vec::new(),
            infer_client_utf8_from_env(),
        ),
        client_size: rmux_os::terminal::current_size(),
    })
}

pub(super) fn parse_new_session(args: &[OsString]) -> Option<NewSessionExtRequest> {
    if has_queue_separator(args) {
        return None;
    }

    let mut session_name = None;
    let mut working_directory = None;
    let mut detached = false;
    let mut cols = None;
    let mut rows = None;
    let mut window_name = None;
    let mut skip_environment_update = false;
    let mut command = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "--" => {
                index += 1;
                command = parse_command_tail(&args[index..])?;
                break;
            }
            "-d" => detached = true,
            "-E" => skip_environment_update = true,
            "-s" => {
                index += 1;
                session_name = Some(SessionName::new(args.get(index)?.to_str()?).ok()?);
            }
            "-n" => {
                index += 1;
                window_name = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-c" => {
                index += 1;
                working_directory = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-x" => {
                index += 1;
                cols = Some(parse_nonzero_u16(args.get(index)?.to_str()?)?);
            }
            "-y" => {
                index += 1;
                rows = Some(parse_nonzero_u16(args.get(index)?.to_str()?)?);
            }
            value if value.starts_with('-') => return None,
            _ => {
                command = parse_command_tail(&args[index..])?;
                break;
            }
        }
        index += 1;
    }

    if !detached {
        return None;
    }

    Some(NewSessionExtRequest {
        session_name,
        working_directory: working_directory.or_else(current_working_directory_string),
        detached: true,
        size: build_terminal_size(cols, rows),
        environment: None,
        group_target: None,
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name,
        print_session_info: false,
        print_format: None,
        command,
        process_command: None,
        client_environment: invoking_client_environment(),
        skip_environment_update,
    })
}

pub(super) fn parse_display_message(args: &[OsString]) -> Option<TinyDisplayMessage> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut format = None;
    let mut message = None;
    let mut print = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "--" => {
                index += 1;
                message = parse_joined_tail(&args[index..])?;
                break;
            }
            "-p" => print = true,
            "-t" => {
                index += 1;
                let raw_target = args.get(index)?.to_str()?;
                if raw_target.contains(':') {
                    return None;
                }
                target = Some(parse_target(raw_target)?);
            }
            "-F" => {
                index += 1;
                format = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-a" | "-I" | "-l" | "-N" | "-v" | "--json" => return None,
            "-c" | "-d" => return None,
            value if value.starts_with('-') => return None,
            _ => {
                message = parse_joined_tail(&args[index..])?;
                break;
            }
        }
        index += 1;
    }

    if !print || (format.is_some() && message.is_some()) {
        return None;
    }

    Some(TinyDisplayMessage {
        target,
        message: format.or(message),
    })
}

pub(super) fn parse_send_keys(args: &[OsString]) -> Option<TinySendKeys> {
    if has_queue_separator(args) {
        return None;
    }

    let mut target = None;
    let mut keys = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "--" => {
                index += 1;
                keys = parse_string_tail(&args[index..])?;
                break;
            }
            "-t" => {
                index += 1;
                target = Some(parse_pane_target(args.get(index)?.to_str()?)?);
            }
            "-F" | "-H" | "-l" | "-K" | "-M" | "-p" | "-R" | "-X" => return None,
            "-N" | "-c" => return None,
            value if value.starts_with('-') => return None,
            _ => {
                keys = parse_string_tail(&args[index..])?;
                break;
            }
        }
        index += 1;
    }

    Some(TinySendKeys {
        target: target?,
        keys,
    })
}

pub(super) fn parse_source_file(args: &[OsString]) -> Option<TinySourceFile> {
    if nested_multiplexer_context() || has_queue_separator(args) {
        return None;
    }

    let paths = match args.first().and_then(|arg| arg.to_str())? {
        "--" => parse_source_paths(&args[1..])?,
        "-F" | "-n" | "-q" | "-v" | "-t" => return None,
        value if value.starts_with('-') => return None,
        _ => parse_source_paths(args)?,
    };

    Some(TinySourceFile {
        paths: (!paths.is_empty()).then_some(paths)?,
    })
}

pub(super) fn parse_resize_pane(args: &[OsString]) -> Option<ResizePaneTargetActionRequest> {
    if has_queue_separator(args) {
        return None;
    }

    let normalized = args
        .iter()
        .map(|arg| arg.to_str().map(ToOwned::to_owned))
        .collect::<Option<Vec<_>>>()?;
    let args = rmux_core::tmux_precedence::normalize_tmux_precedence("resize-pane", normalized)
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();

    let mut target = None;
    let mut columns = None;
    let mut rows = None;
    let mut relative = None;
    let mut zoom = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_str()?;
        match arg {
            "-t" => {
                index += 1;
                target = Some(args.get(index)?.to_str()?.to_owned());
            }
            "-x" => {
                index += 1;
                columns = Some(args.get(index)?.to_str()?.parse::<u16>().ok()?);
            }
            "-y" => {
                index += 1;
                rows = Some(args.get(index)?.to_str()?.parse::<u16>().ok()?);
            }
            "-L" => {
                if relative.is_some() {
                    return None;
                }
                let (cells, consumed) = parse_optional_relative_cells(&args, index)?;
                if reject_trailing_args_after_direction_delta(&args, index, consumed) {
                    return None;
                }
                relative = Some((ResizePaneRelativeDirection::Left, cells));
                index += consumed;
            }
            "-R" => {
                if relative.is_some() {
                    return None;
                }
                let (cells, consumed) = parse_optional_relative_cells(&args, index)?;
                if reject_trailing_args_after_direction_delta(&args, index, consumed) {
                    return None;
                }
                relative = Some((ResizePaneRelativeDirection::Right, cells));
                index += consumed;
            }
            "-U" => {
                if relative.is_some() {
                    return None;
                }
                let (cells, consumed) = parse_optional_relative_cells(&args, index)?;
                if reject_trailing_args_after_direction_delta(&args, index, consumed) {
                    return None;
                }
                relative = Some((ResizePaneRelativeDirection::Up, cells));
                index += consumed;
            }
            "-D" => {
                if relative.is_some() {
                    return None;
                }
                let (cells, consumed) = parse_optional_relative_cells(&args, index)?;
                if reject_trailing_args_after_direction_delta(&args, index, consumed) {
                    return None;
                }
                relative = Some((ResizePaneRelativeDirection::Down, cells));
                index += consumed;
            }
            "-Z" => zoom = true,
            _ => return None,
        }
        index += 1;
    }

    let adjustment = if zoom {
        ResizePaneAdjustment::Zoom
    } else {
        resize_adjustment(columns, rows, relative)
    };
    Some(ResizePaneTargetActionRequest { target, adjustment })
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

fn parse_capture_bound(value: &str) -> Option<(Option<i64>, bool)> {
    if value == "-" {
        return Some((None, true));
    }
    value.parse::<i64>().ok().map(|value| (Some(value), false))
}

fn parse_command_tail(args: &[OsString]) -> Option<Option<Vec<String>>> {
    if args.is_empty() {
        return None;
    }
    args.iter()
        .map(|arg| arg.to_str().map(ToOwned::to_owned))
        .collect::<Option<Vec<_>>>()
        .map(Some)
}

fn parse_joined_tail(args: &[OsString]) -> Option<Option<String>> {
    if args.is_empty() {
        return None;
    }
    parse_string_tail(args).map(|values| Some(values.join(" ")))
}

fn parse_string_tail(args: &[OsString]) -> Option<Vec<String>> {
    args.iter()
        .map(|arg| arg.to_str().map(ToOwned::to_owned))
        .collect()
}

fn parse_source_paths(args: &[OsString]) -> Option<Vec<String>> {
    let paths = parse_string_tail(args)?;
    paths.iter().all(|path| path != "-").then_some(paths)
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

fn parse_optional_relative_cells(args: &[OsString], flag_index: usize) -> Option<(u16, usize)> {
    let Some(next) = args.get(flag_index + 1).and_then(|arg| arg.to_str()) else {
        return Some((1, 0));
    };
    if next.starts_with('-') {
        return Some((1, 0));
    }
    let cells = next.parse::<u16>().ok()?;
    (cells != 0).then_some((cells, 1))
}

fn reject_trailing_args_after_direction_delta(
    args: &[OsString],
    flag_index: usize,
    consumed: usize,
) -> bool {
    consumed != 0 && flag_index + 1 + consumed < args.len()
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

fn parse_nonzero_u16(value: &str) -> Option<u16> {
    let value = value.parse::<u16>().ok()?;
    (value != 0).then_some(value)
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

fn nested_multiplexer_context() -> bool {
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
    windows_client_shell_for_parent_name(&parent_name)
}

#[cfg(windows)]
pub(super) fn windows_client_shell_for_parent_name(parent_name: &str) -> Option<String> {
    let lower = parent_name.to_ascii_lowercase();
    match lower.as_str() {
        "cmd.exe" | "cmd" => Some(
            env::var_os("COMSPEC")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "cmd.exe".into())
                .to_string_lossy()
                .into_owned(),
        ),
        "powershell.exe" | "powershell" => {
            if windows_command_available_on_path("pwsh.exe") {
                Some("pwsh.exe".to_owned())
            } else {
                Some("powershell.exe".to_owned())
            }
        }
        "pwsh.exe" | "pwsh" => Some("pwsh.exe".to_owned()),
        "bash.exe" | "bash" | "sh.exe" | "sh" | "zsh.exe" | "zsh" | "nu.exe" | "nu" => {
            Some(parent_name.to_owned())
        }
        _ => None,
    }
}

#[cfg(windows)]
fn windows_command_available_on_path(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|directory| directory.join(name).is_file())
}

#[cfg(not(windows))]
fn invoking_client_environment() -> Option<Vec<String>> {
    None
}
