//! `send-keys` request payloads.

use serde::{Deserialize, Serialize};

use crate::PaneTarget;

/// Request payload for `send-keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendKeysRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// Key tokens in left-to-right order.
    pub keys: Vec<String>,
}

/// Extended request payload for `send-keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendKeysExtRequest {
    /// The optional explicit pane target.
    pub target: Option<PaneTarget>,
    /// Key tokens in left-to-right order.
    pub keys: Vec<String>,
    /// Whether tmux format expansion should be applied to each token first.
    pub expand_formats: bool,
    /// Whether each token should be interpreted as a hexadecimal byte value.
    pub hex: bool,
    /// Whether tokens should be sent as literal bytes instead of key names.
    #[serde(default)]
    pub literal: bool,
    /// Whether keys should be dispatched through the client's key table.
    pub dispatch_key_table: bool,
    /// Whether tokens describe copy-mode commands.
    pub copy_mode_command: bool,
    /// Whether the payload should be treated as a mouse event.
    pub forward_mouse_event: bool,
    /// Whether the target terminal should be reset before sending keys.
    pub reset_terminal: bool,
    /// Optional tmux repeat count for command or key dispatch.
    pub repeat_count: Option<usize>,
}

/// Further-extended request payload for `send-keys -c`.
///
/// This is intentionally separate from [`SendKeysExtRequest`] so the original
/// bincode field order remains wire-compatible with older clients and daemons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendKeysExt2Request {
    /// The optional explicit pane target.
    pub target: Option<PaneTarget>,
    /// Key tokens in left-to-right order.
    pub keys: Vec<String>,
    /// Whether tmux format expansion should be applied to each token first.
    pub expand_formats: bool,
    /// Whether each token should be interpreted as a hexadecimal byte value.
    pub hex: bool,
    /// Whether tokens should be sent as literal bytes instead of key names.
    #[serde(default)]
    pub literal: bool,
    /// Whether keys should be dispatched through the client's key table.
    pub dispatch_key_table: bool,
    /// Whether tokens describe copy-mode commands.
    pub copy_mode_command: bool,
    /// Whether the payload should be treated as a mouse event.
    pub forward_mouse_event: bool,
    /// Whether the target terminal should be reset before sending keys.
    pub reset_terminal: bool,
    /// Optional tmux repeat count for command or key dispatch.
    pub repeat_count: Option<usize>,
    /// Optional target client used for current-pane resolution and client key dispatch.
    pub target_client: Option<String>,
}
