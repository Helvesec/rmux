use rmux_core::{key_code_lookup_bits, key_string_lookup_key};
use rmux_proto::{
    ErrorResponse, OptionName, PaneTarget, Response, RmuxError, SendKeysResponse, SessionName,
    Target,
};
use rmux_pty::PtyMaster;

use super::display_message_context;
use crate::format_runtime::render_runtime_template;
use crate::input_keys::{encode_key, encode_mouse_event, ExtendedKeyFormat};
use crate::keys::parse_key_code;
use crate::pane_terminals::{session_not_found, HandlerState};

pub(super) struct PaneInputWrite {
    session_name: SessionName,
    window_index: u32,
    pane_index: u32,
    sink: PaneInputSink,
}

enum PaneInputSink {
    Pty(PtyMaster),
    #[cfg(test)]
    CapturedForTest,
}

pub(super) fn prepare_pane_input_write(
    state: &HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
) -> Result<PaneInputWrite, RmuxError> {
    let session_name = target.session_name().clone();
    let window_index = target.window_index();
    let pane_index = target.pane_index();
    let master = state.pane_master_in_window(&session_name, window_index, pane_index)?;
    #[cfg(not(test))]
    let _ = bytes;
    #[cfg(test)]
    if state.append_pane_input_capture_for_test(target, bytes) {
        return Ok(PaneInputWrite {
            session_name,
            window_index,
            pane_index,
            sink: PaneInputSink::CapturedForTest,
        });
    }
    Ok(PaneInputWrite {
        session_name,
        window_index,
        pane_index,
        sink: PaneInputSink::Pty(master),
    })
}

pub(super) fn prepare_attached_pane_input_writes(
    state: &HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
) -> Result<Vec<PaneInputWrite>, RmuxError> {
    prepare_synchronized_pane_input_writes(state, target, bytes)
}

pub(super) fn prepare_synchronized_pane_input_writes(
    state: &HandlerState,
    target: &PaneTarget,
    bytes: &[u8],
) -> Result<Vec<PaneInputWrite>, RmuxError> {
    synchronized_input_targets(state, target)?
        .into_iter()
        .map(|target| prepare_pane_input_write(state, &target, bytes))
        .collect()
}

fn synchronized_input_targets(
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
        .filter(|(_, pane_id)| !state.pane_is_dead(session_name, *pane_id))
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

pub(super) async fn write_bytes_to_target_io(
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
        PaneInputSink::Pty(master) => write_pane_bytes(master, bytes).await.map_err(|error| {
            RmuxError::Server(format!(
                "failed to write to pane {}:{}.{}: {}",
                session_name, window_index, pane_index, error
            ))
        }),
        #[cfg(test)]
        PaneInputSink::CapturedForTest => Ok(()),
    }
}

#[cfg(any(unix, windows))]
async fn write_pane_bytes(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || master.write_all(&bytes))
        .await
        .map_err(|error| std::io::Error::other(format!("pane write task failed: {error}")))?
}

#[cfg(not(any(unix, windows)))]
async fn write_pane_bytes(master: PtyMaster, bytes: Vec<u8>) -> std::io::Result<()> {
    master.write_all(&bytes)
}

pub(in crate::handler) async fn write_bracketed_pane_payload(
    master: PtyMaster,
    payload: Vec<u8>,
    bracketed: bool,
) -> std::io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        tokio::task::spawn_blocking(move || {
            write_bracketed_pane_payload_blocking(&master, &payload, bracketed)
        })
        .await
        .map_err(|error| std::io::Error::other(format!("pane paste task failed: {error}")))?
    }

    #[cfg(not(any(unix, windows)))]
    {
        write_bracketed_pane_payload_blocking(&master, &payload, bracketed)
    }
}

fn write_bracketed_pane_payload_blocking(
    master: &PtyMaster,
    payload: &[u8],
    bracketed: bool,
) -> std::io::Result<()> {
    if bracketed {
        master.write_all(b"\x1b[200~")?;
    }
    master.write_all(payload)?;
    if bracketed {
        master.write_all(b"\x1b[201~")?;
    }
    Ok(())
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
    let format =
        ExtendedKeyFormat::parse(state.options.resolve(None, OptionName::ExtendedKeysFormat));
    Ok(encode_key(pane_mode, format, key))
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
    state: &HandlerState,
    target: &PaneTarget,
    tokens: &[String],
    expand_formats: bool,
) -> Result<Vec<String>, RmuxError> {
    if !expand_formats {
        return Ok(tokens.to_vec());
    }

    let (_, runtime) = display_message_context(state, &Target::Pane(target.clone()), 0)?;
    Ok(tokens
        .iter()
        .map(|token| render_runtime_template(token, &runtime, false))
        .collect())
}
