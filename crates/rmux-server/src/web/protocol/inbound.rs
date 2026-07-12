//! Inbound dispatch: decode client text/binary frames and drive the matching
//! handler/session action. Mutating and input actions fail closed for spectators.

use std::io;

use rmux_core::PaneId;

use super::{
    encode_session_key, encode_session_windows_console_key, parse_pane_resize_body,
    parse_resize_body, session_logout_allowed, valid_window_name, ClientMessage,
    SessionClientTextOutcome, SessionOperatorBinaryOutcome, SessionScrollRequest,
    OPERATOR_INPUT_FRAME_MAX, WS_ATTACH_INPUT, WS_INPUT_KEY, WS_INPUT_TEXT, WS_RESIZE_REQUEST,
    WS_SESSION_RESIZE_PANE,
};
use crate::handler::{RequestHandler, WebPaneStream, WebSessionStream};
use crate::web::outbound::WebSocketOutbound;

pub(crate) async fn handle_pane_client_text(
    socket: &WebSocketOutbound,
    _pane: &mut WebPaneStream,
    text: &str,
) -> io::Result<()> {
    let message = serde_json::from_str::<ClientMessage>(text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    match message {
        ClientMessage::Logout => {
            let _ = socket
                .write_close_code(4006, "logout_requires_session")
                .await;
            Ok(())
        }
        ClientMessage::PaneScroll { .. } => {
            let _ = socket
                .write_close_code(4006, "scroll_requires_session")
                .await;
            Ok(())
        }
        ClientMessage::SelectPane { .. } => {
            let _ = socket
                .write_close_code(4006, "select_pane_requires_session")
                .await;
            Ok(())
        }
        ClientMessage::SplitPane { .. }
        | ClientMessage::NewWindow
        | ClientMessage::KillPane
        | ClientMessage::SelectWindow { .. }
        | ClientMessage::RenameWindow { .. }
        | ClientMessage::KillWindow { .. } => {
            let _ = socket
                .write_close_code(4006, "session_action_requires_session")
                .await;
            Ok(())
        }
    }
}

pub(crate) async fn handle_session_client_text(
    handler: &RequestHandler,
    socket: &WebSocketOutbound,
    session: &mut WebSessionStream,
    text: &str,
) -> io::Result<SessionClientTextOutcome> {
    let message = serde_json::from_str::<ClientMessage>(text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    match message {
        ClientMessage::PaneScroll { pane_id, delta } => {
            if delta == 0 || delta.unsigned_abs() > 10_000 {
                let _ = socket.write_close_code(4006, "invalid_scroll_delta").await;
                return Ok(SessionClientTextOutcome::None);
            }
            Ok(SessionClientTextOutcome::Scroll(SessionScrollRequest {
                pane_id,
                delta,
            }))
        }
        ClientMessage::SelectPane { pane_id } if session.is_operator() => {
            handler
                .web_session_select_pane(
                    session.target(),
                    session.attach_pid(),
                    PaneId::new(pane_id),
                )
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::SelectPane { .. } => {
            let _ = socket
                .write_close_code(4006, "select_pane_requires_operator")
                .await;
            Ok(SessionClientTextOutcome::None)
        }
        ClientMessage::SplitPane { direction } if session.is_operator() => {
            if let Err(error) = handler
                .web_session_split_pane(session.target(), session.attach_pid(), direction.into())
                .await
            {
                tracing::debug!(?error, "web session split pane ignored");
            }
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::NewWindow if session.is_operator() => {
            if let Err(error) = handler
                .web_session_new_window(session.target(), session.attach_pid())
                .await
            {
                tracing::debug!(?error, "web session new window ignored");
            }
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::KillPane if session.is_operator() => {
            if let Err(error) = handler
                .web_session_kill_active_pane(session.target(), session.attach_pid())
                .await
            {
                tracing::debug!(?error, "web session kill pane ignored");
            }
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::SelectWindow { window_index } if session.is_operator() => {
            if let Err(error) = handler
                .web_session_select_window(session.target(), session.attach_pid(), window_index)
                .await
            {
                tracing::debug!(?error, "web session select window ignored");
            }
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::SelectWindow { window_index } => {
            match handler
                .web_session_select_window_for_view(
                    session.target(),
                    session.attach_pid(),
                    window_index,
                )
                .await
            {
                Ok(true) => session.select_window_for_view(window_index),
                Ok(false) => tracing::debug!(window_index, "web session select window ignored"),
                Err(error) => tracing::debug!(?error, "web session select window ignored"),
            }
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::RenameWindow { window_index, name } if session.is_operator() => {
            if !valid_window_name(&name) {
                let _ = socket.write_close_code(4006, "invalid_window_name").await;
                return Ok(SessionClientTextOutcome::None);
            }
            if let Err(error) = handler
                .web_session_rename_window(
                    session.target(),
                    session.attach_pid(),
                    window_index,
                    name,
                )
                .await
            {
                tracing::debug!(?error, "web session rename window ignored");
            }
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::KillWindow { window_index } if session.is_operator() => {
            if let Err(error) = handler
                .web_session_kill_window(session.target(), session.attach_pid(), window_index)
                .await
            {
                tracing::debug!(?error, "web session kill window ignored");
            }
            Ok(SessionClientTextOutcome::Snapshot)
        }
        ClientMessage::SplitPane { .. }
        | ClientMessage::NewWindow
        | ClientMessage::KillPane
        | ClientMessage::RenameWindow { .. }
        | ClientMessage::KillWindow { .. } => {
            let _ = socket
                .write_close_code(4006, "session_action_requires_operator")
                .await;
            Ok(SessionClientTextOutcome::None)
        }
        ClientMessage::Logout
            if session_logout_allowed(session.is_operator(), session.controls()) =>
        {
            handler
                .web_session_logout(session.target(), session.attach_pid())
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            socket.write_close_code(1000, "session_closed").await?;
            Ok(SessionClientTextOutcome::None)
        }
        ClientMessage::Logout if session.is_operator() => {
            let _ = socket
                .write_close_code(4006, "logout_requires_controls")
                .await;
            Ok(SessionClientTextOutcome::None)
        }
        ClientMessage::Logout => {
            let _ = socket
                .write_close_code(4006, "logout_requires_operator")
                .await;
            Ok(SessionClientTextOutcome::None)
        }
    }
}

pub(crate) async fn handle_pane_operator_binary_frame(
    handler: &RequestHandler,
    socket: &WebSocketOutbound,
    pane: &WebPaneStream,
    payload: &[u8],
) -> io::Result<()> {
    let Some((opcode, body)) = parse_operator_frame(socket, payload).await? else {
        return Ok(());
    };
    match opcode {
        WS_INPUT_TEXT => send_pane_text(handler, socket, pane, body).await?,
        WS_INPUT_KEY => send_pane_key(handler, socket, pane, body).await?,
        WS_RESIZE_REQUEST => {
            let _ = socket
                .write_close_code(4006, "web_resize_unsupported")
                .await;
        }
        _ => {
            let _ = socket
                .write_close_code(4006, "unknown_operator_opcode")
                .await;
        }
    }
    Ok(())
}

pub(crate) async fn handle_session_operator_binary_frame(
    handler: &RequestHandler,
    socket: &WebSocketOutbound,
    session: &mut WebSessionStream,
    payload: &[u8],
) -> io::Result<SessionOperatorBinaryOutcome> {
    let Some((opcode, body)) = parse_operator_frame(socket, payload).await? else {
        return Ok(SessionOperatorBinaryOutcome::None);
    };
    let outcome = match opcode {
        WS_INPUT_TEXT => {
            send_session_text(socket, session, body).await?;
            SessionOperatorBinaryOutcome::None
        }
        WS_INPUT_KEY => {
            send_session_key(socket, session, body).await?;
            SessionOperatorBinaryOutcome::None
        }
        WS_ATTACH_INPUT => {
            send_session_attach_input(socket, session, body).await?;
            SessionOperatorBinaryOutcome::None
        }
        WS_RESIZE_REQUEST => resize_session(socket, session, body).await?,
        WS_SESSION_RESIZE_PANE => resize_session_pane(handler, socket, session, body).await?,
        _ => {
            let _ = socket
                .write_close_code(4006, "unknown_operator_opcode")
                .await;
            SessionOperatorBinaryOutcome::None
        }
    };
    Ok(outcome)
}

async fn parse_operator_frame<'a>(
    socket: &WebSocketOutbound,
    payload: &'a [u8],
) -> io::Result<Option<(u8, &'a [u8])>> {
    if payload.is_empty() {
        let _ = socket.write_close_code(4006, "empty_operator_frame").await;
        return Ok(None);
    }
    if payload.len() > OPERATOR_INPUT_FRAME_MAX {
        let _ = socket
            .write_close_code(4002, "operator_frame_too_large")
            .await;
        return Ok(None);
    }
    Ok(Some((payload[0], &payload[1..])))
}

async fn send_pane_text(
    handler: &RequestHandler,
    socket: &WebSocketOutbound,
    pane: &WebPaneStream,
    body: &[u8],
) -> io::Result<()> {
    let Ok(text) = std::str::from_utf8(body) else {
        let _ = socket.write_close_code(4006, "invalid_utf8").await;
        return Ok(());
    };
    let target = handler
        .current_web_pane_target(pane.session_id(), pane.target())
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    handler
        .web_send_text(&target, text.to_owned())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn send_session_text(
    socket: &WebSocketOutbound,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<()> {
    let Ok(text) = std::str::from_utf8(body) else {
        let _ = socket.write_close_code(4006, "invalid_utf8").await;
        return Ok(());
    };
    session
        .send_attach_keystroke(text.as_bytes().to_vec())
        .await
}

async fn send_pane_key(
    handler: &RequestHandler,
    socket: &WebSocketOutbound,
    pane: &WebPaneStream,
    body: &[u8],
) -> io::Result<()> {
    let Some(key) = validate_key_token(socket, body).await? else {
        return Ok(());
    };
    let target = handler
        .current_web_pane_target(pane.session_id(), pane.target())
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    handler
        .web_send_key(&target, key.to_owned())
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn send_session_key(
    socket: &WebSocketOutbound,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<()> {
    let Some(key) = validate_key_token(socket, body).await? else {
        return Ok(());
    };
    if let Some((bytes, windows_key)) = encode_session_windows_console_key(key) {
        return session
            .send_attach_windows_console_key(bytes, windows_key)
            .await;
    }
    let Some(bytes) = encode_session_key(key) else {
        let _ = socket.write_close_code(4006, "unsupported_key_token").await;
        return Ok(());
    };
    session.send_attach_keystroke(bytes).await
}

async fn validate_key_token<'a>(
    socket: &WebSocketOutbound,
    body: &'a [u8],
) -> io::Result<Option<&'a str>> {
    let Ok(key) = std::str::from_utf8(body) else {
        let _ = socket.write_close_code(4006, "invalid_key_utf8").await;
        return Ok(None);
    };
    if key.len() > 64
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        let _ = socket.write_close_code(4006, "invalid_key_token").await;
        return Ok(None);
    }
    Ok(Some(key))
}

async fn send_session_attach_input(
    socket: &WebSocketOutbound,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<()> {
    if !session.controls() {
        let _ = socket.write_close_code(4006, "controls_not_enabled").await;
        return Ok(());
    }
    session.send_attach_keystroke(body.to_vec()).await
}

async fn resize_session(
    socket: &WebSocketOutbound,
    session: &mut WebSessionStream,
    body: &[u8],
) -> io::Result<SessionOperatorBinaryOutcome> {
    if !session.is_resize_authority() {
        return Ok(SessionOperatorBinaryOutcome::None);
    }
    let Some(size) = parse_resize_body(body) else {
        let _ = socket
            .write_close_code(4006, "invalid_resize_request")
            .await;
        return Ok(SessionOperatorBinaryOutcome::None);
    };
    session.send_attach_resize(size).await?;
    Ok(SessionOperatorBinaryOutcome::Resize)
}

async fn resize_session_pane(
    handler: &RequestHandler,
    socket: &WebSocketOutbound,
    session: &WebSessionStream,
    body: &[u8],
) -> io::Result<SessionOperatorBinaryOutcome> {
    let Some((pane_id, adjustment)) = parse_pane_resize_body(body) else {
        let _ = socket
            .write_close_code(4006, "invalid_pane_resize_request")
            .await;
        return Ok(SessionOperatorBinaryOutcome::None);
    };
    if let Err(error) = handler
        .web_session_resize_pane(
            session.target(),
            session.attach_pid(),
            PaneId::new(pane_id),
            adjustment,
        )
        .await
    {
        tracing::debug!(?error, "web session pane resize ignored");
        return Ok(SessionOperatorBinaryOutcome::None);
    }
    Ok(SessionOperatorBinaryOutcome::Snapshot)
}
