use rmux_proto::{ErrorResponse, PaneTarget, Response, RmuxError, SendKeysResponse, SessionName};

use super::super::RequestHandler;
use super::pane_io_encoding::{
    prepare_pane_console_input_write, synchronized_input_targets,
    tokens_emulate_windows_cmd_select_all, tokens_route_windows_control_as_pty_bytes,
    windows_console_input_for_target_tokens, windows_console_input_for_token,
    write_windows_console_input_action_to_target_io, PaneConsoleInputWrite,
    WindowsConsoleInputAction,
};
use super::{
    encode_tokens_for_target, prepare_pane_input_write, write_bytes_to_targets, PaneInputWrite,
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
    if tokens.is_empty() || !tokens_contain_windows_console_input(tokens) {
        return Ok(None);
    }

    let repeat_count = bounded_repeat_count(repeat_count);
    let mut steps = Vec::with_capacity(tokens.len().saturating_mul(repeat_count));
    for _ in 0..repeat_count {
        for token in tokens {
            for input_target in input_targets_for_scope(state, target, scope)? {
                let single_token = [token.clone()];
                if !tokens_emulate_windows_cmd_select_all(state, &input_target, &single_token) {
                    if let Some((action, console_bytes)) = windows_console_input_for_target_tokens(
                        state,
                        &input_target,
                        &single_token,
                        1,
                    ) {
                        if tokens_route_windows_control_as_pty_bytes(
                            state,
                            &input_target,
                            &single_token,
                        ) {
                            let write =
                                prepare_pane_input_write(state, &input_target, &console_bytes)?;
                            steps.push(PreparedWindowsConsoleInputStep::Bytes {
                                writes: vec![write],
                                bytes: console_bytes,
                            });
                        } else {
                            let wrote_bytes = !console_bytes.is_empty();
                            let write = prepare_pane_console_input_write(
                                state,
                                &input_target,
                                &console_bytes,
                                action,
                            )?;
                            steps.push(PreparedWindowsConsoleInputStep::Console {
                                writes: vec![write],
                                action,
                                wrote_bytes,
                            });
                        }
                        continue;
                    }
                }

                let bytes = encode_tokens_for_target(state, &input_target, &single_token)?;
                let write = prepare_pane_input_write(state, &input_target, &bytes)?;
                steps.push(PreparedWindowsConsoleInputStep::Bytes {
                    writes: vec![write],
                    bytes,
                });
            }
        }
    }

    Ok(Some(steps))
}

fn tokens_contain_windows_console_input(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|token| windows_console_input_for_token(token, 1).is_some())
}

fn input_targets_for_scope(
    state: &HandlerState,
    target: &PaneTarget,
    scope: WindowsConsoleInputScope,
) -> Result<Vec<PaneTarget>, RmuxError> {
    match scope {
        WindowsConsoleInputScope::Synchronized => synchronized_input_targets(state, target),
        WindowsConsoleInputScope::SinglePane => Ok(vec![target.clone()]),
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
