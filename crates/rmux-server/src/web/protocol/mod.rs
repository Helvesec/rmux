//! Web-share wire protocol (v1, X25519 + ML-KEM-768 hybrid).
//!
//! This module owns the shared vocabulary — constants, the client/auth wire
//! types, and the small parse/validate helpers — and re-exports the directional
//! logic split across submodules by responsibility:
//!
//! - [`handshake`]: pre-ready hello / challenge / encrypted auth.
//! - [`inbound`]: decode and dispatch client text/binary frames.
//! - [`outbound`]: frame terminal output and the JSON server messages.
//!
//! Submodules are descendants of this module, so they read these private items
//! directly via `super::` without elevating their visibility.

use serde::Deserialize;
use std::time::Duration;

use rmux_proto::{ResizePaneAdjustment, SplitDirection, TerminalSize};

use crate::input_keys::{encode_key, ExtendedKeyFormat};
use crate::keys::parse_key_code;
use crate::web::crypto::E2EE_CAPABILITY;

mod handshake;
mod inbound;
mod outbound;
#[cfg(test)]
mod tests;

pub(crate) use handshake::{
    build_challenge, close_for_auth_error, read_auth_message, read_client_hello, send_text,
};
pub(crate) use inbound::{
    handle_pane_client_text, handle_pane_operator_binary_frame, handle_session_client_text,
    handle_session_operator_binary_frame,
};
pub(crate) use outbound::{
    queue_output, queue_resize, queue_session_snapshot, queue_session_view, queue_snapshot,
    send_ready, send_revoked, send_viewer_count,
};

pub(crate) const PRE_AUTH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const AUTH_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const UNIFORM_AUTH_DELAY: Duration = Duration::from_millis(50);

pub(crate) const WEB_SHARE_PROTOCOL_VERSION: u16 = 1;

/// The single wire close pair used for EVERY pre-ready handshake rejection.
///
/// A web-share relay performing its own DH knows both DH secrets, so only the
/// token authenticates the channel and the PIN is a secondary factor.
/// Distinguishable close codes (e.g. capacity-reached, which implies a correct
/// PIN) would leak a PIN/identity oracle, so all pre-ready failures collapse to
/// this one pair. The precise reason is logged server-side, never sent.
pub(crate) const HANDSHAKE_REJECTED: (u16, &str) = (4000, "handshake_rejected");
const SERVER_CAPABILITIES: &[&str] = &[E2EE_CAPABILITY, "terminal-palette-v1"];
const OPERATOR_INPUT_FRAME_MAX: usize = 4 * 1024;
const MAX_PANE_RESIZE_CELLS: u16 = 10_000;
const WS_OUTPUT_RAW: u8 = 0x01;
const WS_RESIZE_NOTIFY: u8 = 0x02;
const WS_SNAPSHOT_FULL: u8 = 0x10;
const WS_SESSION_VIEW: u8 = 0x11;
const WS_INPUT_TEXT: u8 = 0x80;
const WS_INPUT_KEY: u8 = 0x81;
const WS_RESIZE_REQUEST: u8 = 0x82;
const WS_ATTACH_INPUT: u8 = 0x83;
const WS_SESSION_RESIZE_PANE: u8 = 0x84;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionScrollRequest {
    pub(crate) pane_id: u32,
    pub(crate) delta: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionOperatorBinaryOutcome {
    None,
    Snapshot,
}

#[derive(Debug)]
pub(crate) struct AuthMessage {
    pub(crate) pin: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthWireMessage {
    #[serde(rename = "type")]
    kind: String,
    protocol_version: u16,
    capabilities: Vec<String>,
    #[serde(default)]
    pin: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Logout,
    PaneScroll { pane_id: u32, delta: i32 },
    SelectPane { pane_id: u32 },
    SplitPane { direction: ClientSplitDirection },
    NewWindow,
    KillPane,
    SelectWindow { window_index: u32 },
    RenameWindow { window_index: u32, name: String },
    KillWindow { window_index: u32 },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClientSplitDirection {
    Horizontal,
    Vertical,
}

impl From<ClientSplitDirection> for SplitDirection {
    fn from(value: ClientSplitDirection) -> Self {
        match value {
            ClientSplitDirection::Horizontal => Self::Horizontal,
            ClientSplitDirection::Vertical => Self::Vertical,
        }
    }
}

fn parse_resize_body(body: &[u8]) -> Option<TerminalSize> {
    if body.len() != 4 {
        return None;
    }
    let cols = u16::from_be_bytes([body[0], body[1]]);
    let rows = u16::from_be_bytes([body[2], body[3]]);
    (cols > 0 && rows > 0).then_some(TerminalSize { cols, rows })
}

fn parse_pane_resize_body(body: &[u8]) -> Option<(u32, ResizePaneAdjustment)> {
    if body.len() != 7 {
        return None;
    }
    let pane_id = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
    let cells = u16::from_be_bytes([body[5], body[6]]);
    if cells == 0 || cells > MAX_PANE_RESIZE_CELLS {
        return None;
    }
    let adjustment = match body[4] {
        0 => ResizePaneAdjustment::Left { cells },
        1 => ResizePaneAdjustment::Right { cells },
        2 => ResizePaneAdjustment::Up { cells },
        3 => ResizePaneAdjustment::Down { cells },
        _ => return None,
    };
    Some((pane_id, adjustment))
}

fn session_logout_allowed(is_operator: bool, controls: bool) -> bool {
    is_operator && controls
}

fn valid_window_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

fn encode_session_key(token: &str) -> Option<Vec<u8>> {
    let key = parse_key_code(token)?;
    encode_key(0, ExtendedKeyFormat::Xterm, key)
}
