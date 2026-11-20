use rmux_core::PaneId;
use rmux_proto::{OptionName, PaneTarget, RmuxError, SessionName};
use rmux_pty::PtyMaster;
#[cfg(windows)]
use rmux_pty::{ProcessId, WindowsConsoleKeyEvent};

#[cfg(windows)]
use crate::pane_terminals::DeferredInitialPaneConsoleInputAction;
use crate::pane_terminals::{session_not_found, HandlerState, PasteDelimiters};

mod committed_focus;
mod key_encoding;
#[cfg(windows)]
mod windows_console_keys;
#[cfg(windows)]
mod windows_target_shell;
mod write_failure;
mod write_io;

pub(in crate::handler) use committed_focus::prepare_committed_focus_input_write;
pub(super) use key_encoding::{
    encode_key_for_target, encode_mouse_for_target, encode_tokens_for_target, pane_input_mode,
};
#[cfg(windows)]
pub(super) use windows_console_keys::{
    should_emulate_windows_cmd_select_all, should_route_windows_control_as_pty_bytes,
    tokens_emulate_windows_cmd_select_all, tokens_route_windows_control_as_pty_bytes,
    windows_console_input_for_attached_key, windows_console_input_for_target_tokens,
    windows_console_input_for_token,
};
pub(in crate::handler) use write_io::{
    write_attached_bytes_to_target_io, write_bytes_to_target_io,
};
#[cfg(windows)]
pub(super) use write_io::{
    write_attached_windows_console_input_action_to_target_io,
    write_windows_console_input_action_to_target_io,
};
pub(super) use write_io::{write_bytes_to_target, write_bytes_to_targets};

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
    #[cfg(windows)]
    ConsoleUtf8(ProcessId),
    #[cfg(windows)]
    OrderedConsoleUtf8 {
        master: PtyMaster,
        pid: ProcessId,
    },
    Disabled,
    #[cfg(windows)]
    QueuedStarting,
    #[cfg(test)]
    CapturedForTest,
}

#[cfg(windows)]
pub(in crate::handler) const LEGACY_CONPTY_NON_UTF8_BRACKETED_PASTE_ERROR: &str =
    "cannot preserve bracketed paste containing non-UTF-8 bytes on this Windows host";

/// Where a pasted body is written on Windows.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::handler) enum WindowsPasteSink {
    /// The pane's ConPTY input pipe.
    Pty,
    /// Console text records written straight into the pane's input buffer.
    ConsoleUtf8,
    /// The payload cannot be represented as console records and would lose its
    /// envelope on the pipe.
    RejectNonUtf8,
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
    prepare_pane_input_write_with_encoding(state, target, bytes, liveness, None)
}

/// Prepares a write for a pasted body, which must reach the destination byte
/// for byte.
///
/// `delimiters` says whether the payload carries the bracketed-paste envelope
/// the destination announced. It is not what selects the Windows sink — a
/// legacy ConPTY consumes control sequences from the raw input pipe whether or
/// not an envelope surrounds them — only what an unrepresentable payload means.
pub(in crate::handler) fn prepare_pane_bracketed_paste_write(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    liveness: PaneInputLiveness,
    delimiters: PasteDelimiters,
) -> Result<PaneInputWrite, RmuxError> {
    prepare_pane_input_write_with_encoding(state, target, bytes, liveness, Some(delimiters))
}

fn prepare_pane_input_write_with_encoding(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    liveness: PaneInputLiveness,
    paste: Option<PasteDelimiters>,
) -> Result<PaneInputWrite, RmuxError> {
    let pane_id = pane_id_for_input_target(state, target)?;
    prepare_resolved_pane_input_write(state, target, bytes, liveness, paste, pane_id)
}

fn prepare_resolved_pane_input_write(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    liveness: PaneInputLiveness,
    paste: Option<PasteDelimiters>,
    pane_id: PaneId,
) -> Result<PaneInputWrite, RmuxError> {
    let session_name = target.session_name().clone();
    let window_index = target.window_index();
    let pane_index = target.pane_index();
    if state.pane_input_is_disabled(pane_id) || state.pane_is_empty(pane_id) {
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
    if if let Some(delimiters) = paste {
        state.queue_starting_pane_paste_input(
            &session_name,
            window_index,
            pane_index,
            bytes,
            delimiters,
        )?
    } else {
        state.queue_starting_pane_input(&session_name, window_index, pane_index, bytes)?
    } {
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
    #[cfg(windows)]
    if let Some(delimiters) = paste {
        match windows_paste_sink(master.preserves_verbatim_input(), bytes, delimiters) {
            WindowsPasteSink::Pty => {}
            WindowsPasteSink::ConsoleUtf8 => {
                let raw_pid = state.pane_pid_in_window(&session_name, window_index, pane_index)?;
                let pid = ProcessId::new(raw_pid)
                    .map_err(|error| RmuxError::Server(error.to_string()))?;
                return Ok(PaneInputWrite {
                    session_name,
                    window_index,
                    pane_index,
                    sink: PaneInputSink::ConsoleUtf8(pid),
                });
            }
            WindowsPasteSink::RejectNonUtf8 => {
                return Err(RmuxError::Message(
                    LEGACY_CONPTY_NON_UTF8_BRACKETED_PASTE_ERROR.to_owned(),
                ));
            }
        }
    }
    #[cfg(not(windows))]
    let _ = paste;
    #[cfg(not(any(test, windows)))]
    let _ = bytes;
    Ok(PaneInputWrite {
        session_name,
        window_index,
        pane_index,
        sink: PaneInputSink::Pty(master),
    })
}

/// Chooses the sink a pasted body reaches its destination through.
///
/// The decision is the host's ConPTY capability, never the destination's own
/// bracketed-paste mode: a legacy build's input state machine parses and
/// consumes control sequences from the raw pipe, so an unaware destination
/// loses a pasted `ESC[…` exactly as an aware one loses its delimiters. Only a
/// payload the console records cannot carry still depends on `delimiters`.
#[cfg(windows)]
pub(in crate::handler) fn windows_paste_sink(
    preserves_verbatim_input: bool,
    bytes: &[u8],
    delimiters: PasteDelimiters,
) -> WindowsPasteSink {
    if preserves_verbatim_input {
        return WindowsPasteSink::Pty;
    }
    if std::str::from_utf8(bytes).is_ok() {
        return WindowsPasteSink::ConsoleUtf8;
    }
    match delimiters {
        // The console path is the only one that keeps an envelope on this
        // host, so a paste it cannot represent must be refused rather than
        // silently delivered to the destination as live input.
        PasteDelimiters::Wrapped => WindowsPasteSink::RejectNonUtf8,
        // A bare body has no envelope to lose. Refusing it would turn input
        // that the byte-oriented path has always carried into an error.
        PasteDelimiters::Bare => WindowsPasteSink::Pty,
    }
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
    let targets = synchronized_input_targets(state, target)?;
    super::super::with_expected_synchronized_input_targets(
        state,
        target,
        targets,
        |state, targets| {
            targets
                .into_iter()
                .map(|target| prepare_pane_console_input_write(state, &target, bytes, action))
                .collect()
        },
    )
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

pub(super) fn prepare_synchronized_pane_input_writes(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
) -> Result<Vec<PaneInputWrite>, RmuxError> {
    let targets = synchronized_input_targets(state, target)?;
    super::super::with_expected_synchronized_input_targets(
        state,
        target,
        targets,
        |state, targets| {
            targets
                .into_iter()
                .map(|target| {
                    prepare_pane_input_write(state, &target, bytes, PaneInputLiveness::TolerateDead)
                })
                .collect()
        },
    )
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

pub(super) fn pane_id_for_input_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<rmux_core::PaneId, RmuxError> {
    super::super::attach_support::check_queued_input_target(state, target)?;
    pane_id_for_existing_target(state, target)
}

fn pane_id_for_existing_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<PaneId, RmuxError> {
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

#[cfg(all(test, windows))]
mod windows_tests {
    use crate::pane_terminals::{DeferredInitialPaneConsoleInputAction, PasteDelimiters};
    use rmux_pty::WindowsConsoleKeyEvent;

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
    fn legacy_conpty_routes_valid_utf8_and_rejects_unrepresentable_paste() {
        let bracketed = b"\x1b[200~alpha\r\nbeta\x1b[201~";

        assert_eq!(
            super::windows_paste_sink(false, bracketed, PasteDelimiters::Wrapped),
            super::WindowsPasteSink::ConsoleUtf8
        );
        assert_eq!(
            super::windows_paste_sink(true, bracketed, PasteDelimiters::Wrapped),
            super::WindowsPasteSink::Pty,
            "passthrough ConPTY must retain the byte-oriented path"
        );
        assert_eq!(
            super::windows_paste_sink(
                false,
                b"\x1b[200~invalid \xff\x1b[201~",
                PasteDelimiters::Wrapped
            ),
            super::WindowsPasteSink::RejectNonUtf8,
            "legacy ConPTY must not silently strip the paste delimiters"
        );
    }

    /// A legacy ConPTY's input state machine parses and consumes the control
    /// sequences inside a pasted body, so a destination that never announced
    /// `?2004h` loses `ESC[9;2u` exactly as an aware one loses its delimiters.
    /// Selecting the sink from the destination's mode is what left the bare
    /// body on the raw pipe.
    #[test]
    fn a_bare_paste_body_takes_the_same_console_sink_as_a_wrapped_one() {
        let body = "alpha\r\n\u{1b}[9;2u \u{1b}[<64;2;2M omega".as_bytes();

        assert_eq!(
            super::windows_paste_sink(false, body, PasteDelimiters::Bare),
            super::WindowsPasteSink::ConsoleUtf8,
            "a bare body must not be left on the sequence-consuming raw pipe"
        );
        assert_eq!(
            super::windows_paste_sink(true, body, PasteDelimiters::Bare),
            super::WindowsPasteSink::Pty,
            "a passthrough ConPTY already carries the body verbatim"
        );
    }

    /// The asymmetry the disposition exists for: refusing a paste protects an
    /// envelope this host cannot otherwise keep, and a bare body has none. It
    /// must therefore keep the path that has always carried it rather than
    /// become an error.
    #[test]
    fn a_bare_paste_body_that_is_not_utf8_keeps_the_byte_oriented_path() {
        assert_eq!(
            super::windows_paste_sink(false, b"invalid \xff bytes", PasteDelimiters::Bare),
            super::WindowsPasteSink::Pty
        );
    }
}
