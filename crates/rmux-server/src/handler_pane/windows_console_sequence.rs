use rmux_proto::{ErrorResponse, PaneTarget, Response, RmuxError, SendKeysResponse, SessionName};

use super::super::RequestHandler;
use super::pane_io_encoding::{
    prepare_attached_pane_console_input_writes, prepare_pane_console_input_write,
    tokens_emulate_windows_cmd_select_all, tokens_route_windows_control_as_pty_bytes,
    windows_console_input_for_token, write_windows_console_input_action_to_target_io,
    PaneConsoleInputWrite, WindowsConsoleInputAction,
};
use super::{
    encode_tokens_for_target, prepare_pane_input_write, prepare_synchronized_pane_input_writes,
    write_bytes_to_targets, PaneInputWrite,
};
use crate::limits::bounded_repeat_count;
use crate::pane_terminals::HandlerState;

pub(super) enum PreparedWindowsConsoleInputStep {
    Bytes {
        writes: Vec<PaneInputWrite>,
        bytes: Vec<u8>,
    },
    Console {
        writes: Vec<PaneConsoleInputWrite>,
        action: WindowsConsoleInputAction,
        wrote_bytes: bool,
    },
}

pub(super) fn prepare_windows_console_input_sequence(
    state: &mut HandlerState,
    target: &PaneTarget,
    tokens: &[String],
    repeat_count: Option<usize>,
) -> Result<Option<Vec<PreparedWindowsConsoleInputStep>>, RmuxError> {
    prepare_windows_console_input_sequence_for_scope(
        state,
        target,
        tokens,
        repeat_count,
        WindowsConsoleInputScope::Synchronized,
    )
}

pub(super) fn prepare_single_pane_windows_console_input_sequence(
    state: &mut HandlerState,
    target: &PaneTarget,
    tokens: &[String],
    repeat_count: Option<usize>,
) -> Result<Option<Vec<PreparedWindowsConsoleInputStep>>, RmuxError> {
    prepare_windows_console_input_sequence_for_scope(
        state,
        target,
        tokens,
        repeat_count,
        WindowsConsoleInputScope::SinglePane,
    )
}

#[derive(Clone, Copy)]
enum WindowsConsoleInputScope {
    Synchronized,
    SinglePane,
}

fn prepare_windows_console_input_sequence_for_scope(
    state: &mut HandlerState,
    target: &PaneTarget,
    tokens: &[String],
    repeat_count: Option<usize>,
    scope: WindowsConsoleInputScope,
) -> Result<Option<Vec<PreparedWindowsConsoleInputStep>>, RmuxError> {
    if tokens.len() <= 1 || !tokens_contain_windows_console_input(tokens) {
        return Ok(None);
    }

    let repeat_count = bounded_repeat_count(repeat_count);
    let mut steps = Vec::with_capacity(tokens.len().saturating_mul(repeat_count));
    for _ in 0..repeat_count {
        for token in tokens {
            let single_token = [token.clone()];
            if !tokens_emulate_windows_cmd_select_all(state, target, &single_token) {
                if let Some((action, console_bytes)) = windows_console_input_for_token(token, 1) {
                    if tokens_route_windows_control_as_pty_bytes(state, target, &single_token) {
                        let writes = prepare_bytes_writes(state, target, &console_bytes, scope)?;
                        steps.push(PreparedWindowsConsoleInputStep::Bytes {
                            writes,
                            bytes: console_bytes,
                        });
                    } else {
                        let writes =
                            prepare_console_writes(state, target, &console_bytes, action, scope)?;
                        steps.push(PreparedWindowsConsoleInputStep::Console {
                            writes,
                            action,
                            wrote_bytes: !console_bytes.is_empty(),
                        });
                    }
                    continue;
                }
            }

            let bytes = encode_tokens_for_target(state, target, &single_token)?;
            let writes = prepare_bytes_writes(state, target, &bytes, scope)?;
            steps.push(PreparedWindowsConsoleInputStep::Bytes { writes, bytes });
        }
    }

    Ok(Some(steps))
}

fn tokens_contain_windows_console_input(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| windows_console_input_for_token(token, 1).is_some())
}

fn prepare_bytes_writes(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    scope: WindowsConsoleInputScope,
) -> Result<Vec<PaneInputWrite>, RmuxError> {
    match scope {
        WindowsConsoleInputScope::Synchronized => {
            prepare_synchronized_pane_input_writes(state, target, bytes)
        }
        WindowsConsoleInputScope::SinglePane => {
            Ok(vec![prepare_pane_input_write(state, target, bytes)?])
        }
    }
}

fn prepare_console_writes(
    state: &mut HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
    action: WindowsConsoleInputAction,
    scope: WindowsConsoleInputScope,
) -> Result<Vec<PaneConsoleInputWrite>, RmuxError> {
    match scope {
        WindowsConsoleInputScope::Synchronized => {
            prepare_attached_pane_console_input_writes(state, target, bytes, action)
        }
        WindowsConsoleInputScope::SinglePane => Ok(vec![prepare_pane_console_input_write(
            state, target, bytes, action,
        )?]),
    }
}

impl RequestHandler {
    pub(super) async fn write_windows_console_input_sequence_and_mark_interactive(
        &self,
        steps: Vec<PreparedWindowsConsoleInputStep>,
        key_count: usize,
    ) -> Response {
        let mut interactive_sessions = Vec::new();
        for step in steps {
            match step {
                PreparedWindowsConsoleInputStep::Bytes { writes, bytes } => {
                    if !bytes.is_empty() {
                        push_unique_sessions(
                            &mut interactive_sessions,
                            input_write_sessions(&writes),
                        );
                    }
                    let response = write_bytes_to_targets(writes, bytes, key_count).await;
                    if !matches!(response, Response::SendKeys(_)) {
                        return response;
                    }
                }
                PreparedWindowsConsoleInputStep::Console {
                    writes,
                    action,
                    wrote_bytes,
                } => {
                    if wrote_bytes {
                        push_unique_sessions(
                            &mut interactive_sessions,
                            console_input_write_sessions(&writes),
                        );
                    }
                    for write in writes {
                        if let Err(error) =
                            write_windows_console_input_action_to_target_io(write, action).await
                        {
                            return Response::Error(ErrorResponse { error });
                        }
                    }
                }
            }
        }
        for session_name in interactive_sessions {
            self.mark_attached_session_interactive_input(&session_name)
                .await;
        }
        Response::SendKeys(SendKeysResponse { key_count })
    }
}

fn input_write_sessions(writes: &[PaneInputWrite]) -> Vec<SessionName> {
    let mut sessions = Vec::new();
    for write in writes {
        let session_name = write.session_name();
        if !sessions.iter().any(|existing| existing == session_name) {
            sessions.push(session_name.clone());
        }
    }
    sessions
}

fn console_input_write_sessions(writes: &[PaneConsoleInputWrite]) -> Vec<SessionName> {
    let mut sessions = Vec::new();
    for write in writes {
        let session_name = write.session_name();
        if !sessions.iter().any(|existing| existing == session_name) {
            sessions.push(session_name.clone());
        }
    }
    sessions
}

fn push_unique_sessions(sessions: &mut Vec<SessionName>, new_sessions: Vec<SessionName>) {
    for session_name in new_sessions {
        if !sessions.iter().any(|existing| existing == &session_name) {
            sessions.push(session_name);
        }
    }
}
