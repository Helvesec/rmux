#[cfg(windows)]
use rmux_core::key_string_lookup_string;
use std::io;

use rmux_core::{key_code_lookup_bits, key_string_lookup_key};
#[cfg(windows)]
use rmux_proto::ProcessCommand;
use rmux_proto::{
    ErrorResponse, OptionName, PaneTarget, Response, RmuxError, SendKeysResponse, SessionName,
};
use rmux_pty::PtyMaster;
#[cfg(windows)]
use rmux_pty::{ProcessId, WindowsConsoleKeyEvent};

use crate::input_keys::{encode_key, encode_mouse_event, ExtendedKeyFormat};
use crate::keys::parse_key_code;
#[cfg(windows)]
use crate::pane_terminals::DeferredInitialPaneConsoleInputAction;
use crate::pane_terminals::{session_not_found, HandlerState};

#[cfg(unix)]
const IMMEDIATE_PANE_INPUT_MAX_BYTES: usize = 256;

pub(in crate::handler) struct PaneInputWrite {
    session_name: SessionName,
    window_index: u32,
    pane_index: u32,
    sink: PaneInputSink,
}

impl PaneInputWrite {
    pub(super) fn session_name(&self) -> &SessionName {
        &self.session_name
    }
}

enum PaneInputSink {
    Pty(PtyMaster),
    Disabled,
    #[cfg(windows)]
    QueuedStarting,
    #[cfg(test)]
    CapturedForTest,
}

#[cfg(windows)]
pub(super) struct PaneConsoleInputWrite {
    session_name: SessionName,
    window_index: u32,
    pane_index: u32,
    sink: PaneConsoleInputSink,
}

#[cfg(windows)]
impl PaneConsoleInputWrite {
    pub(super) fn session_name(&self) -> &SessionName {
        &self.session_name
    }
}

#[cfg(windows)]
enum PaneConsoleInputSink {
    ConsolePid(ProcessId),
    Disabled,
    QueuedStarting,
    #[cfg(test)]
    CapturedForTest,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WindowsConsoleInputAction {
    Key(WindowsConsoleKeyEvent),
    KeyThenInterrupt(WindowsConsoleKeyEvent),
    Interrupt,
}

#[cfg(windows)]
impl WindowsConsoleInputAction {
    const fn deferred(self) -> DeferredInitialPaneConsoleInputAction {
        match self {
            Self::Key(key) => DeferredInitialPaneConsoleInputAction::Key(key),
            Self::KeyThenInterrupt(key) => {
                DeferredInitialPaneConsoleInputAction::KeyThenInterrupt(key)
            }
            Self::Interrupt => DeferredInitialPaneConsoleInputAction::Interrupt,
        }
    }
}

/// Whether resolving a pane input write should treat an exited child process
/// as an error. Paste-buffer rejects dead remain-on-exit panes like tmux;
/// attached input and send-keys must not use child-process liveness as a pane
/// liveness gate (dead-pane write errors are tolerated downstream instead).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum PaneInputLiveness {
    TolerateDead,
    RejectDead,
}

pub(in crate::handler) fn prepare_pane_input_write(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    liveness: PaneInputLiveness,
) -> Result<PaneInputWrite, RmuxError> {
    let session_name = target.session_name().clone();
    let window_index = target.window_index();
    let pane_index = target.pane_index();
    let pane_id = pane_id_for_input_target(state, target)?;
    if state.pane_input_is_disabled(pane_id) {
        #[cfg(not(test))]
        let _ = bytes;
        return Ok(PaneInputWrite {
            session_name,
            window_index,
            pane_index,
            sink: PaneInputSink::Disabled,
        });
    }
    #[cfg(test)]
    if state.append_pane_input_capture_for_test(target, bytes) {
        return Ok(PaneInputWrite {
            session_name,
            window_index,
            pane_index,
            sink: PaneInputSink::CapturedForTest,
        });
    }
    #[cfg(windows)]
    if state.queue_starting_pane_input(&session_name, window_index, pane_index, bytes)? {
        return Ok(PaneInputWrite {
            session_name,
            window_index,
            pane_index,
            sink: PaneInputSink::QueuedStarting,
        });
    }
    let master = match liveness {
        PaneInputLiveness::RejectDead => {
            state.clone_pane_master_if_alive(&session_name, window_index, pane_index)?
        }
        PaneInputLiveness::TolerateDead => {
            state.clone_pane_master(&session_name, window_index, pane_index)?
        }
    };
    #[cfg(not(any(test, windows)))]
    let _ = bytes;
    Ok(PaneInputWrite {
        session_name,
        window_index,
        pane_index,
        sink: PaneInputSink::Pty(master),
    })
}

pub(super) fn prepare_attached_pane_input_writes(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
) -> Result<Vec<PaneInputWrite>, RmuxError> {
    prepare_synchronized_pane_input_writes(state, target, bytes)
}

#[cfg(windows)]
pub(super) fn prepare_attached_pane_console_input_writes(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    action: WindowsConsoleInputAction,
) -> Result<Vec<PaneConsoleInputWrite>, RmuxError> {
    synchronized_input_targets(state, target)?
        .into_iter()
        .map(|target| prepare_pane_console_input_write(state, &target, bytes, action))
        .collect()
}

#[cfg(windows)]
pub(super) fn prepare_pane_console_input_write(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    action: WindowsConsoleInputAction,
) -> Result<PaneConsoleInputWrite, RmuxError> {
    let session_name = target.session_name().clone();
    let window_index = target.window_index();
    let pane_index = target.pane_index();
    let pane_id = pane_id_for_input_target(state, target)?;
    if state.pane_input_is_disabled(pane_id) {
        let _ = bytes;
        return Ok(PaneConsoleInputWrite {
            session_name,
            window_index,
            pane_index,
            sink: PaneConsoleInputSink::Disabled,
        });
    }
    #[cfg(test)]
    if state.append_pane_input_capture_for_test(target, bytes) {
        return Ok(PaneConsoleInputWrite {
            session_name,
            window_index,
            pane_index,
            sink: PaneConsoleInputSink::CapturedForTest,
        });
    }
    if state.queue_starting_pane_console_input(
        &session_name,
        window_index,
        pane_index,
        action.deferred(),
        bytes.len(),
    )? {
        return Ok(PaneConsoleInputWrite {
            session_name,
            window_index,
            pane_index,
            sink: PaneConsoleInputSink::QueuedStarting,
        });
    }
    let raw_pid = state.pane_pid_in_window(&session_name, window_index, pane_index)?;
    let pid = ProcessId::new(raw_pid).map_err(|error| RmuxError::Server(error.to_string()))?;
    Ok(PaneConsoleInputWrite {
        session_name,
        window_index,
        pane_index,
        sink: PaneConsoleInputSink::ConsolePid(pid),
    })
}

#[cfg(windows)]
pub(super) fn windows_console_input_for_attached_key(
    state: &HandlerState,
    target: &PaneTarget,
    decoded_key: rmux_core::KeyCode,
    console_key: WindowsConsoleKeyEvent,
) -> WindowsConsoleInputAction {
    let console_key = if key_matches_name(decoded_key, "C-d")
        && !target_uses_windows_cmd_console_ctrl_d(state, target)
    {
        windows_ctrl_d_console_key(false).with_repeat_count(console_key.repeat_count())
    } else {
        console_key
    };
    windows_console_input_for_attached_key_event(console_key)
}

#[cfg(windows)]
fn windows_console_input_for_attached_key_event(
    console_key: WindowsConsoleKeyEvent,
) -> WindowsConsoleInputAction {
    if console_key.virtual_key_code() == b'C' as u16 && console_key.unicode_char() == 0x03 {
        WindowsConsoleInputAction::KeyThenInterrupt(console_key)
    } else {
        WindowsConsoleInputAction::Key(console_key)
    }
}

#[cfg(windows)]
pub(super) fn windows_console_input_for_tokens(
    tokens: &[String],
    repeat_count: usize,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    windows_console_input_for_tokens_with_ctrl_d(
        tokens,
        repeat_count,
        WindowsConsoleKeyEvent::ctrl_d(),
    )
}

#[cfg(windows)]
pub(super) fn windows_console_input_for_target_tokens(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
    repeat_count: usize,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    let target_uses_cmd =
        tokens_are_windows_ctrl_d(tokens) && target_uses_windows_cmd_console_ctrl_d(state, target);
    let ctrl_d = windows_ctrl_d_console_key(target_uses_cmd);
    windows_console_input_for_tokens_with_ctrl_d(tokens, repeat_count, ctrl_d)
}

#[cfg(windows)]
/// `cmd.exe` needs the physical key record to interrupt commands such as
/// `timeout.exe`. Every other target receives a typed, scan-code-free EOT: the
/// writer suppresses it in processed mode but preserves it for raw/TUI input.
const fn windows_ctrl_d_console_key(target_uses_cmd: bool) -> WindowsConsoleKeyEvent {
    if target_uses_cmd {
        WindowsConsoleKeyEvent::ctrl_d()
    } else {
        WindowsConsoleKeyEvent::ctrl_d_eot()
    }
}

#[cfg(windows)]
fn windows_console_input_for_tokens_with_ctrl_d(
    tokens: &[String],
    repeat_count: usize,
    ctrl_d: WindowsConsoleKeyEvent,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    let [token] = tokens else {
        return None;
    };
    windows_console_input_for_token_with_ctrl_d(token, repeat_count, ctrl_d)
}

#[cfg(windows)]
pub(super) fn windows_console_input_for_token(
    token: &str,
    repeat_count: usize,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    windows_console_input_for_token_with_ctrl_d(
        token,
        repeat_count,
        WindowsConsoleKeyEvent::ctrl_d(),
    )
}

#[cfg(windows)]
fn windows_console_input_for_token_with_ctrl_d(
    token: &str,
    repeat_count: usize,
    ctrl_d: WindowsConsoleKeyEvent,
) -> Option<(WindowsConsoleInputAction, Vec<u8>)> {
    let key = key_code_lookup_bits(parse_key_code(token)?);
    let repeat_count = repeat_count.min(usize::from(u16::MAX)).max(1);
    let repeat_count_u16 = repeat_count as u16;
    let (key_event, byte) = windows_console_ctrl_letter_for_key(key, ctrl_d)?;
    let key_event = key_event.with_repeat_count(repeat_count_u16);
    let action = if key_matches_name(key, "C-c") {
        WindowsConsoleInputAction::KeyThenInterrupt(key_event)
    } else {
        WindowsConsoleInputAction::Key(key_event)
    };
    Some((action, vec![byte; repeat_count]))
}

#[cfg(windows)]
fn tokens_are_windows_ctrl_d(tokens: &[String]) -> bool {
    let [token] = tokens else {
        return false;
    };
    parse_key_code(token).is_some_and(|key| key_matches_name(key_code_lookup_bits(key), "C-d"))
}

#[cfg(windows)]
pub(super) fn tokens_contain_windows_console_interrupt(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        windows_console_input_for_token(token, 1).is_some_and(|(action, _)| {
            matches!(action, WindowsConsoleInputAction::KeyThenInterrupt(_))
        })
    })
}

#[cfg(windows)]
fn windows_console_ctrl_letter_for_key(
    key: rmux_core::KeyCode,
    ctrl_d: WindowsConsoleKeyEvent,
) -> Option<(WindowsConsoleKeyEvent, u8)> {
    for letter in b'A'..=b'Z' {
        let name = format!("C-{}", char::from(letter.to_ascii_lowercase()));
        if Some(key) == key_string_lookup_string(&name).map(key_code_lookup_bits) {
            let event = match letter {
                b'C' => WindowsConsoleKeyEvent::ctrl_c(),
                b'D' => ctrl_d,
                b'Z' => WindowsConsoleKeyEvent::ctrl_z(),
                _ => WindowsConsoleKeyEvent::ctrl_letter(letter)?,
            };
            return Some((event, letter - b'A' + 1));
        }
    }
    None
}

#[cfg(windows)]
pub(super) fn should_emulate_windows_cmd_select_all(
    state: &HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
) -> bool {
    key_matches_name(key, "C-a") && target_uses_windows_cmd_shell(state, target)
}

#[cfg(windows)]
pub(super) fn tokens_emulate_windows_cmd_select_all(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
) -> bool {
    let [token] = tokens else {
        return false;
    };
    parse_key_code(token)
        .is_some_and(|key| should_emulate_windows_cmd_select_all(state, target, key))
}

#[cfg(windows)]
pub(super) fn should_route_windows_control_as_pty_bytes(
    state: &HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
) -> bool {
    if key_matches_name(key, "C-d") {
        return target_routes_windows_ctrl_d_as_posix_eot(state, target);
    }
    key_matches_name(key, "C-c") && target_uses_wsl_host_process(state, target)
}

#[cfg(windows)]
pub(super) fn tokens_route_windows_control_as_pty_bytes(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
) -> bool {
    let [token] = tokens else {
        return false;
    };
    parse_key_code(token)
        .is_some_and(|key| should_route_windows_control_as_pty_bytes(state, target, key))
}

#[cfg(windows)]
fn target_uses_windows_cmd_shell(state: &HandlerState, target: &PaneTarget) -> bool {
    let profile_shell_is_cmd = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(|profile| profile.shell().file_name())
        .and_then(|name| name.to_str())
        .is_some_and(is_windows_cmd_name);
    if profile_shell_is_cmd {
        return true;
    }

    state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(rmux_os::process::command_name)
        .as_deref()
        .is_some_and(is_windows_cmd_name)
}

#[cfg(windows)]
fn target_uses_windows_cmd_console_ctrl_d(state: &HandlerState, target: &PaneTarget) -> bool {
    if let Some(uses_cmd) = pane_id_for_input_target(state, target)
        .ok()
        .and_then(|pane_id| state.pane_start_process_command_for_id(pane_id))
        .and_then(process_command_windows_cmd_hint)
    {
        return uses_cmd;
    }

    if let Some(profile_shell_is_cmd) = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(|profile| {
            profile
                .shell()
                .file_name()
                .and_then(|name| name.to_str())
                .map(is_windows_cmd_name)
        })
    {
        return profile_shell_is_cmd;
    }

    state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(rmux_os::process::command_name)
        .as_deref()
        .is_some_and(is_windows_cmd_name)
}

#[cfg(windows)]
fn process_command_windows_cmd_hint(command: &ProcessCommand) -> Option<bool> {
    match command {
        // Shell text is normally executed by the configured shell, but an
        // explicit nested shell overrides that profile for Ctrl-D semantics.
        ProcessCommand::Shell(command) => shell_text_windows_cmd_hint(command),
        ProcessCommand::Argv(argv) => Some(argv_invokes_windows_cmd(argv)),
        _ => None,
    }
}

#[cfg(windows)]
fn shell_text_windows_cmd_hint(command: &str) -> Option<bool> {
    let head = windows_shell_command_head(command)?;
    let name = windows_command_name(head);
    if is_windows_cmd_name(name) {
        Some(true)
    } else if matches!(
        name.to_ascii_lowercase().as_str(),
        "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe" | "wsl" | "wsl.exe"
    ) {
        Some(false)
    } else {
        None
    }
}

#[cfg(windows)]
fn windows_shell_command_head(command: &str) -> Option<&str> {
    let command = command.trim_start();
    let quote = command.as_bytes().first().copied()?;
    if matches!(quote, b'"' | b'\'') {
        let quoted = &command[1..];
        let end = quoted.as_bytes().iter().position(|byte| *byte == quote)?;
        return Some(&quoted[..end]);
    }
    let end = command.find(char::is_whitespace).unwrap_or(command.len());
    Some(&command[..end])
}

#[cfg(windows)]
fn argv_invokes_windows_cmd(argv: &[String]) -> bool {
    let Some(head) = argv.first() else {
        return false;
    };
    let name = windows_command_name(head);
    is_windows_cmd_name(name)
        || std::path::Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
}

#[cfg(windows)]
fn windows_command_name(command: &str) -> &str {
    let trimmed = command.trim_matches(['"', '\'']);
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(trimmed)
}

#[cfg(windows)]
fn target_uses_wsl_host_process(state: &HandlerState, target: &PaneTarget) -> bool {
    let lifecycle_command = pane_id_for_input_target(state, target)
        .ok()
        .and_then(|pane_id| state.pane_start_command_for_id(pane_id));
    let lifecycle_matches = lifecycle_command.is_some_and(command_invokes_wsl);
    let process_name = state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(rmux_os::process::command_name);
    let process_matches = process_name.as_deref().is_some_and(is_wsl_host_name);
    trace_windows_wsl_detection(process_name.as_deref(), lifecycle_matches, process_matches);
    if lifecycle_matches {
        return true;
    }

    process_matches
}

#[cfg(windows)]
fn target_routes_windows_ctrl_d_as_posix_eot(state: &HandlerState, target: &PaneTarget) -> bool {
    target_uses_wsl_host_process(state, target) || target_has_wsl_descendant_process(state, target)
}

#[cfg(windows)]
fn target_has_wsl_descendant_process(state: &HandlerState, target: &PaneTarget) -> bool {
    let Some(pane_pid) = state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
    else {
        return false;
    };

    let descendant_matches = rmux_os::process::descendant_command_names(pane_pid)
        .iter()
        .any(|name| is_wsl_host_name(name));
    trace_windows_wsl_descendant_detection(pane_pid, descendant_matches);
    descendant_matches
}

#[cfg(windows)]
fn trace_windows_wsl_descendant_detection(pane_pid: u32, descendant_matches: bool) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        pane_pid,
        descendant_matches,
        "detect Windows WSL descendant control-key routing"
    );
}

#[cfg(windows)]
fn trace_windows_wsl_detection(
    process_name: Option<&str>,
    lifecycle_matches: bool,
    process_matches: bool,
) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        process_name,
        lifecycle_matches,
        process_matches,
        "detect Windows WSL control-key routing"
    );
}

#[cfg(windows)]
fn command_invokes_wsl(command: &[String]) -> bool {
    command.iter().any(|part| {
        part.split_whitespace()
            .next()
            .is_some_and(is_wsl_command_head)
            || part.to_ascii_lowercase().contains("wsl.exe")
    })
}

#[cfg(windows)]
fn is_wsl_command_head(head: &str) -> bool {
    let trimmed = head.trim_matches(['"', '\'']);
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_wsl_host_name)
        || is_wsl_host_name(trimmed)
}

#[cfg(windows)]
fn is_wsl_host_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "wsl.exe"
        || lower == "wsl"
        || lower.ends_with("\\wsl.exe")
        || lower.ends_with("/wsl.exe")
}

#[cfg(windows)]
fn is_windows_cmd_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("cmd.exe") || name.eq_ignore_ascii_case("cmd")
}

#[cfg(windows)]
fn windows_cmd_select_all_sequence(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<Option<Vec<u8>>, RmuxError> {
    let mut bytes = Vec::new();
    for key_name in ["C-Home", "S-End"] {
        let Some(key) = key_string_lookup_string(key_name) else {
            return Ok(None);
        };
        let Some(encoded) = encode_key_for_target(state, target, key)? else {
            return Ok(None);
        };
        bytes.extend_from_slice(&encoded);
    }
    Ok(Some(bytes))
}

#[cfg(windows)]
fn key_matches_name(key: rmux_core::KeyCode, name: &str) -> bool {
    key_string_lookup_string(name)
        .is_some_and(|candidate| key_code_lookup_bits(candidate) == key_code_lookup_bits(key))
}

pub(super) fn prepare_synchronized_pane_input_writes(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
) -> Result<Vec<PaneInputWrite>, RmuxError> {
    synchronized_input_targets(state, target)?
        .into_iter()
        .map(|target| {
            prepare_pane_input_write(state, &target, bytes, PaneInputLiveness::TolerateDead)
        })
        .collect()
}

pub(super) fn synchronized_input_targets(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<Vec<PaneTarget>, RmuxError> {
    let session_name = target.session_name();
    let window_index = target.window_index();
    let pane_index = target.pane_index();
    let synchronized =
        state
            .options
            .resolve_for_window(session_name, window_index, OptionName::SynchronizePanes)
            == Some("on");
    let panes = {
        let session = state
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let window = session.window_at(window_index).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{session_name}:{window_index}"),
                "window index does not exist in session",
            )
        })?;
        let Some(target_pane) = window.pane(pane_index) else {
            return Err(RmuxError::invalid_target(
                target.to_string(),
                "pane index does not exist in window",
            ));
        };
        if synchronized {
            window
                .panes()
                .iter()
                .map(|pane| (pane.index(), pane.id()))
                .collect::<Vec<_>>()
        } else {
            vec![(pane_index, target_pane.id())]
        }
    };

    Ok(panes
        .into_iter()
        .filter(|(_, pane_id)| {
            !state.pane_is_dead(session_name, *pane_id) && !state.pane_input_is_disabled(*pane_id)
        })
        .map(|(pane_index, _)| {
            PaneTarget::with_window(session_name.clone(), window_index, pane_index)
        })
        .collect())
}

pub(super) async fn write_bytes_to_target(
    write: PaneInputWrite,
    bytes: Vec<u8>,
    key_count: usize,
) -> Response {
    match write_bytes_to_target_io(write, bytes).await {
        Ok(()) => Response::SendKeys(SendKeysResponse { key_count }),
        Err(error) => Response::Error(ErrorResponse { error }),
    }
}

pub(super) async fn write_bytes_to_targets(
    writes: Vec<PaneInputWrite>,
    bytes: Vec<u8>,
    key_count: usize,
) -> Response {
    for write in writes {
        if let Err(error) = write_bytes_to_target_io(write, bytes.clone()).await {
            return Response::Error(ErrorResponse { error });
        }
    }
    Response::SendKeys(SendKeysResponse { key_count })
}

pub(in crate::handler) async fn write_bytes_to_target_io(
    write: PaneInputWrite,
    bytes: Vec<u8>,
) -> Result<(), RmuxError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let PaneInputWrite {
        session_name,
        window_index,
        pane_index,
        sink,
    } = write;
    match sink {
        PaneInputSink::Disabled => Ok(()),
        #[cfg(windows)]
        PaneInputSink::QueuedStarting => Ok(()),
        PaneInputSink::Pty(master) => match write_pane_bytes(master, bytes).await {
            Ok(()) => Ok(()),
            Err(error) if is_dead_pane_write_error(&error) => {
                Err(RmuxError::Server("target pane has exited".to_owned()))
            }
            Err(error) => Err(RmuxError::Server(format!(
                "failed to write to pane {}:{}.{}: {}",
                session_name, window_index, pane_index, error
            ))),
        },
        #[cfg(test)]
        PaneInputSink::CapturedForTest => Ok(()),
    }
}

pub(in crate::handler) async fn write_attached_bytes_to_target_io(
    write: PaneInputWrite,
    bytes: Vec<u8>,
) -> Result<(), RmuxError> {
    match write_bytes_to_target_io(write, bytes).await {
        Err(error) if is_dead_pane_input_error(&error) => Ok(()),
        result => result,
    }
}

fn is_dead_pane_input_error(error: &RmuxError) -> bool {
    matches!(error, RmuxError::Server(message) if message == "target pane has exited")
}

pub(in crate::handler) fn is_dead_pane_write_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    ) || is_unix_pty_eio(error)
}

#[cfg(unix)]
fn is_unix_pty_eio(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
}

#[cfg(not(unix))]
fn is_unix_pty_eio(_error: &io::Error) -> bool {
    false
}

#[cfg(windows)]
pub(super) async fn write_windows_console_input_action_to_target_io(
    write: PaneConsoleInputWrite,
    action: WindowsConsoleInputAction,
) -> Result<(), RmuxError> {
    let PaneConsoleInputWrite {
        session_name,
        window_index,
        pane_index,
        sink,
    } = write;
    match sink {
        PaneConsoleInputSink::Disabled => Ok(()),
        PaneConsoleInputSink::QueuedStarting => Ok(()),
        PaneConsoleInputSink::ConsolePid(pid) => {
            trace_windows_console_input(
                &session_name,
                window_index,
                pane_index,
                pid,
                action,
                "dispatch",
            );
            tokio::task::spawn_blocking(move || match action {
                WindowsConsoleInputAction::Key(key) => {
                    crate::windows_console_input::write_with_transient_retry(|| {
                        rmux_pty::write_windows_console_key(pid, key)
                    })
                }
                WindowsConsoleInputAction::KeyThenInterrupt(key) => {
                    crate::windows_console_input::write_with_transient_retry(|| {
                        rmux_pty::write_windows_console_key_then_interrupt_if_processed(pid, key)
                    })
                }
                WindowsConsoleInputAction::Interrupt => {
                    crate::windows_console_input::write_with_transient_retry(|| {
                        rmux_pty::send_windows_console_interrupt(pid)
                    })
                }
            })
            .await
            .map_err(|error| RmuxError::Server(format!("pane console input task failed: {error}")))?
            .map_err(|error| {
                RmuxError::Server(format!(
                    "failed to write console input to pane {}:{}.{}: {}",
                    session_name, window_index, pane_index, error
                ))
            })
        }
        #[cfg(test)]
        PaneConsoleInputSink::CapturedForTest => Ok(()),
    }
}

#[cfg(windows)]
pub(super) async fn write_windows_console_key_to_target_io(
    write: PaneConsoleInputWrite,
    key: WindowsConsoleKeyEvent,
) -> Result<(), RmuxError> {
    write_windows_console_input_action_to_target_io(write, WindowsConsoleInputAction::Key(key))
        .await
}

#[cfg(windows)]
fn trace_windows_console_input(
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
    pid: ProcessId,
    action: WindowsConsoleInputAction,
    stage: &'static str,
) {
    if std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_none() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        %session_name,
        window_index,
        pane_index,
        pid = pid.as_u32(),
        ?action,
        stage,
        "Windows console input action"
    );
}

pub(super) fn pane_id_for_input_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<rmux_core::PaneId, RmuxError> {
    let session_name = target.session_name();
    let window_index = target.window_index();
    let pane_index = target.pane_index();
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let window = session.window_at(window_index).ok_or_else(|| {
        RmuxError::invalid_target(
            format!("{session_name}:{window_index}"),
            "window index does not exist in session",
        )
    })?;
    window
        .pane(pane_index)
        .map(rmux_core::Pane::id)
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in window")
        })
}

#[cfg(any(unix, windows))]
async fn write_pane_bytes(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    #[cfg(unix)]
    if should_try_immediate_pane_input(bytes.len()) {
        let written = master.try_write_immediate(&bytes)?;
        if written == bytes.len() {
            return Ok(());
        }
        return write_pane_bytes_blocking(master, bytes[written..].to_vec()).await;
    }

    write_pane_bytes_blocking(master, bytes).await
}

#[cfg(any(unix, windows))]
async fn write_pane_bytes_blocking(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || master.write_all(&bytes))
        .await
        .map_err(|error| std::io::Error::other(format!("pane write task failed: {error}")))?
}

#[cfg(unix)]
fn should_try_immediate_pane_input(byte_len: usize) -> bool {
    (1..=IMMEDIATE_PANE_INPUT_MAX_BYTES).contains(&byte_len)
}

#[cfg(not(any(unix, windows)))]
async fn write_pane_bytes(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    master.write_all(&bytes)
}

pub(super) fn encode_tokens_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
) -> Result<Vec<u8>, RmuxError> {
    let mut bytes = Vec::new();
    for token in tokens {
        if let Some(key) = parse_key_code(token) {
            let Some(encoded) = encode_key_for_target(state, target, key)? else {
                return Err(RmuxError::Server(format!(
                    "key {} cannot be sent to a pane",
                    key_string_lookup_key(key_code_lookup_bits(key), false)
                )));
            };
            bytes.extend_from_slice(&encoded);
        } else {
            bytes.extend_from_slice(token.as_bytes());
        }
    }
    Ok(bytes)
}

pub(super) fn encode_key_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
) -> Result<Option<Vec<u8>>, RmuxError> {
    #[cfg(windows)]
    if should_emulate_windows_cmd_select_all(state, target, key) {
        return windows_cmd_select_all_sequence(state, target);
    }

    let pane_mode = pane_input_mode(state, target)?;
    let format =
        ExtendedKeyFormat::parse(state.options.resolve(None, OptionName::ExtendedKeysFormat));
    Ok(encode_key(pane_mode, format, key))
}

pub(super) fn pane_input_mode(state: &HandlerState, target: &PaneTarget) -> Result<u32, RmuxError> {
    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })?;
    let pane_mode = state
        .pane_screen_state(target.session_name(), pane_id)
        .map(|screen_state| screen_state.mode)
        .unwrap_or_default();
    Ok(pane_mode)
}

pub(super) fn encode_mouse_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    event: &crate::mouse::AttachedMouseEvent,
) -> Result<Vec<u8>, RmuxError> {
    let session = state
        .sessions
        .session(target.session_name())
        .ok_or_else(|| session_not_found(target.session_name()))?;
    let window = session.window_at(target.window_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "window index does not exist in session")
    })?;
    let pane = window.pane(target.pane_index()).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
    })?;
    if event.ignore || event.pane_id != Some(pane.id()) {
        return Ok(Vec::new());
    }

    let pane_mode = state
        .pane_screen_state(target.session_name(), pane.id())
        .map(|screen_state| screen_state.mode)
        .unwrap_or_default();
    let adjusted_y = match event.status_at {
        Some(0) if event.raw.y >= event.status_lines => event.raw.y - event.status_lines,
        _ => event.raw.y,
    };
    if event.raw.x < pane.geometry().x()
        || event.raw.x >= pane.geometry().x().saturating_add(pane.geometry().cols())
        || adjusted_y < pane.geometry().y()
        || adjusted_y >= pane.geometry().y().saturating_add(pane.geometry().rows())
    {
        return Ok(Vec::new());
    }
    let x = event.raw.x - pane.geometry().x();
    let y = adjusted_y - pane.geometry().y();
    Ok(encode_mouse_event(pane_mode, &event.raw, x, y).unwrap_or_default())
}

pub(super) fn expand_send_key_tokens(
    _state: &HandlerState,
    _target: &PaneTarget,
    tokens: &[String],
    _expand_formats: bool,
) -> Result<Vec<String>, RmuxError> {
    Ok(tokens.to_vec())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn immediate_pane_input_is_reserved_for_short_interactive_writes() {
        assert!(!super::should_try_immediate_pane_input(0));
        assert!(super::should_try_immediate_pane_input(1));
        assert!(super::should_try_immediate_pane_input(
            super::IMMEDIATE_PANE_INPUT_MAX_BYTES
        ));
        assert!(!super::should_try_immediate_pane_input(
            super::IMMEDIATE_PANE_INPUT_MAX_BYTES + 1
        ));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use crate::pane_terminals::DeferredInitialPaneConsoleInputAction;
    use rmux_proto::ProcessCommand;
    use rmux_pty::WindowsConsoleKeyEvent;

    #[test]
    fn windows_console_key_mapping_covers_control_signals() {
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-a".to_owned()], 1),
            Some((
                super::WindowsConsoleInputAction::Key(
                    WindowsConsoleKeyEvent::ctrl_letter(b'A').unwrap()
                ),
                vec![0x01]
            ))
        );
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-c".to_owned()], 1),
            Some((
                super::WindowsConsoleInputAction::KeyThenInterrupt(WindowsConsoleKeyEvent::ctrl_c()),
                vec![0x03]
            ))
        );
        assert_eq!(
            super::windows_console_input_for_attached_key_event(WindowsConsoleKeyEvent::ctrl_c()),
            super::WindowsConsoleInputAction::KeyThenInterrupt(WindowsConsoleKeyEvent::ctrl_c())
        );
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-d".to_owned()], 1),
            Some((
                super::WindowsConsoleInputAction::Key(WindowsConsoleKeyEvent::ctrl_d()),
                vec![0x04]
            ))
        );
        assert_eq!(
            super::windows_console_input_for_tokens(&["C-z".to_owned()], 2),
            Some((
                super::WindowsConsoleInputAction::Key(
                    WindowsConsoleKeyEvent::ctrl_z().with_repeat_count(2)
                ),
                vec![0x1a, 0x1a]
            ))
        );
        assert!(super::tokens_contain_windows_console_interrupt(&[
            "Enter".to_owned(),
            "C-c".to_owned()
        ]));
        assert!(super::tokens_contain_windows_console_interrupt(&[
            "C-c".to_owned(),
            "Enter".to_owned()
        ]));
        assert!(!super::tokens_contain_windows_console_interrupt(&[
            "C-d".to_owned(),
            "Enter".to_owned()
        ]));
    }

    #[test]
    fn deferred_windows_console_actions_preserve_control_semantics() {
        assert_eq!(
            super::WindowsConsoleInputAction::Key(WindowsConsoleKeyEvent::ctrl_d()).deferred(),
            DeferredInitialPaneConsoleInputAction::Key(WindowsConsoleKeyEvent::ctrl_d())
        );
        assert_eq!(
            super::WindowsConsoleInputAction::KeyThenInterrupt(WindowsConsoleKeyEvent::ctrl_c())
                .deferred(),
            DeferredInitialPaneConsoleInputAction::KeyThenInterrupt(
                WindowsConsoleKeyEvent::ctrl_c()
            )
        );
        assert_eq!(
            super::WindowsConsoleInputAction::Interrupt.deferred(),
            DeferredInitialPaneConsoleInputAction::Interrupt
        );
    }

    #[test]
    fn windows_ctrl_d_uses_a_physical_key_only_for_cmd() {
        assert_eq!(
            super::windows_ctrl_d_console_key(false),
            WindowsConsoleKeyEvent::ctrl_d_eot()
        );
        assert_eq!(
            super::windows_ctrl_d_console_key(true),
            WindowsConsoleKeyEvent::ctrl_d()
        );
    }

    #[test]
    fn windows_ctrl_d_launch_plan_detection_preserves_shell_wrapper() {
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                "timeout.exe /T 10000".to_owned()
            )),
            None,
            "shell text must defer to the configured shell profile"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                "pwsh.exe -NoLogo -NoProfile".to_owned()
            )),
            Some(false),
            "an explicit PowerShell command must override a cmd profile"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoProfile"#.to_owned()
            )),
            Some(false),
            "a quoted PowerShell path must remain one command token"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                "cmd.exe /D /Q /K".to_owned()
            )),
            Some(true)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Shell(
                r#""C:\Windows\System32\cmd.exe" /D /Q /K"#.to_owned()
            )),
            Some(true),
            "a quoted cmd path must retain physical Ctrl-D semantics"
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                "cmd.exe".to_owned(),
                "/K".to_owned(),
            ])),
            Some(true)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                r"C:\tools\build.cmd".to_owned(),
            ])),
            Some(true)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                "pwsh.exe".to_owned(),
                "-NoProfile".to_owned(),
            ])),
            Some(false)
        );
        assert_eq!(
            super::process_command_windows_cmd_hint(&ProcessCommand::Argv(vec![
                "python.exe".to_owned(),
            ])),
            Some(false)
        );
    }
}
