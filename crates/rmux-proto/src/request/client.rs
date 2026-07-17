use serde::{Deserialize, Serialize};

use crate::{ClientTerminalContext, SessionName, TerminalSize};

/// Request payload for `attach-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachSessionRequest {
    /// The exact target session name.
    pub target: SessionName,
}

/// Extended request payload for `attach-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachSessionExtRequest {
    /// The optional exact target session name.
    pub target: Option<SessionName>,
    /// Whether other attached clients should be detached first.
    #[serde(default)]
    pub detach_other_clients: bool,
    /// Whether other attached clients should be detached and terminated.
    #[serde(default)]
    pub kill_other_clients: bool,
    /// Whether readonly attach mode should be enabled.
    #[serde(default)]
    pub read_only: bool,
    /// Whether client environment updates should be skipped.
    #[serde(default)]
    pub skip_environment_update: bool,
    /// Optional tmux client-flag names such as `read-only` or `active-pane`.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
}

/// Further-extended request payload for `attach-session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachSessionExt2Request {
    /// The optional exact target session name.
    #[serde(default)]
    pub target: Option<SessionName>,
    /// The optional raw tmux-style target text, including window/pane selectors.
    #[serde(default)]
    pub target_spec: Option<String>,
    /// Whether other attached clients should be detached first.
    #[serde(default)]
    pub detach_other_clients: bool,
    /// Whether other attached clients should be detached and terminated.
    #[serde(default)]
    pub kill_other_clients: bool,
    /// Whether readonly attach mode should be enabled.
    #[serde(default)]
    pub read_only: bool,
    /// Whether client environment updates should be skipped.
    #[serde(default)]
    pub skip_environment_update: bool,
    /// Optional tmux client-flag names such as `read-only` or `active-pane`.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
    /// Optional tmux format-expanded working directory applied to the target session.
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Terminal/runtime hints captured from the invoking client.
    #[serde(default)]
    pub client_terminal: ClientTerminalContext,
    /// The invoking client terminal size, when known.
    #[serde(default)]
    pub client_size: Option<TerminalSize>,
}

/// Attach request payload with explicit attach-stream client capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachSessionExt3Request {
    /// The optional exact target session name.
    #[serde(default)]
    pub target: Option<SessionName>,
    /// The optional raw tmux-style target text, including window/pane selectors.
    #[serde(default)]
    pub target_spec: Option<String>,
    /// Whether other attached clients should be detached first.
    #[serde(default)]
    pub detach_other_clients: bool,
    /// Whether other attached clients should be detached and terminated.
    #[serde(default)]
    pub kill_other_clients: bool,
    /// Whether readonly attach mode should be enabled.
    #[serde(default)]
    pub read_only: bool,
    /// Whether client environment updates should be skipped.
    #[serde(default)]
    pub skip_environment_update: bool,
    /// Optional tmux client-flag names such as `read-only` or `active-pane`.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
    /// Optional tmux format-expanded working directory applied to the target session.
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Terminal/runtime hints captured from the invoking client.
    #[serde(default)]
    pub client_terminal: ClientTerminalContext,
    /// The invoking client terminal size, when known.
    #[serde(default)]
    pub client_size: Option<TerminalSize>,
    /// Attach-stream messages this client can decode.
    #[serde(default)]
    pub attach_capabilities: Vec<String>,
}

impl AttachSessionExt3Request {
    /// Builds the capability-aware request from the v2 attach request shape.
    #[must_use]
    pub fn from_ext2(request: AttachSessionExt2Request, attach_capabilities: Vec<String>) -> Self {
        Self {
            target: request.target,
            target_spec: request.target_spec,
            detach_other_clients: request.detach_other_clients,
            kill_other_clients: request.kill_other_clients,
            read_only: request.read_only,
            skip_environment_update: request.skip_environment_update,
            flags: request.flags,
            working_directory: request.working_directory,
            client_terminal: request.client_terminal,
            client_size: request.client_size,
            attach_capabilities,
        }
    }

    /// Splits out the v2 payload fields and the attach-stream capabilities.
    #[must_use]
    pub fn into_ext2_and_capabilities(self) -> (AttachSessionExt2Request, Vec<String>) {
        (
            AttachSessionExt2Request {
                target: self.target,
                target_spec: self.target_spec,
                detach_other_clients: self.detach_other_clients,
                kill_other_clients: self.kill_other_clients,
                read_only: self.read_only,
                skip_environment_update: self.skip_environment_update,
                flags: self.flags,
                working_directory: self.working_directory,
                client_terminal: self.client_terminal,
                client_size: self.client_size,
            },
            self.attach_capabilities,
        )
    }
}

/// Request payload for `switch-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchClientRequest {
    /// The exact target session name.
    pub target: SessionName,
}

/// Extended request payload for `switch-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchClientExtRequest {
    /// The optional exact target session name.
    pub target: Option<SessionName>,
    /// The optional key table to set for the attached client.
    pub key_table: Option<String>,
}

/// Further-extended request payload for `switch-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchClientExt2Request {
    /// The optional exact target session name.
    pub target: Option<SessionName>,
    /// The optional key table to set for the attached client.
    #[serde(default)]
    pub key_table: Option<String>,
    /// Whether the client's last session should be recalled.
    #[serde(default)]
    pub last_session: bool,
    /// Whether the next session in order should be selected.
    #[serde(default)]
    pub next_session: bool,
    /// Whether the previous session in order should be selected.
    #[serde(default)]
    pub previous_session: bool,
    /// Whether readonly mode should be toggled for the addressed client.
    #[serde(default)]
    pub toggle_read_only: bool,
    /// Reserved legacy field kept for wire compatibility; `switch-client` does not support `-f`.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
    /// Optional tmux list-sorting token used by `-n` and `-p`.
    #[serde(default)]
    pub sort_order: Option<String>,
    /// Whether client environment updates should be skipped.
    #[serde(default)]
    pub skip_environment_update: bool,
}

/// Further-extended request payload for `switch-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchClientExt3Request {
    /// The optional target-client identifier or `=`.
    #[serde(default)]
    pub target_client: Option<String>,
    /// The optional tmux target string, including pane or window targets.
    #[serde(default)]
    pub target: Option<String>,
    /// The optional key table to set for the attached client.
    #[serde(default)]
    pub key_table: Option<String>,
    /// Whether the client's last session should be recalled.
    #[serde(default)]
    pub last_session: bool,
    /// Whether the next session in order should be selected.
    #[serde(default)]
    pub next_session: bool,
    /// Whether the previous session in order should be selected.
    #[serde(default)]
    pub previous_session: bool,
    /// Whether readonly mode should be toggled for the addressed client.
    #[serde(default)]
    pub toggle_read_only: bool,
    /// Optional tmux list-sorting token used by `-n` and `-p`.
    #[serde(default)]
    pub sort_order: Option<String>,
    /// Whether client environment updates should be skipped.
    #[serde(default)]
    pub skip_environment_update: bool,
    /// Whether zoom should be preserved when switching panes.
    #[serde(default)]
    pub zoom: bool,
}

/// Request payload for `detach-client`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetachClientRequest;

/// Extended request payload for `detach-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetachClientExtRequest {
    /// The optional target-client identifier or `=`.
    #[serde(default)]
    pub target_client: Option<String>,
    /// Whether all other clients should be detached instead.
    #[serde(default)]
    pub all_other_clients: bool,
    /// The optional target session whose clients should all be detached.
    #[serde(default)]
    pub target_session: Option<SessionName>,
    /// Whether targeted clients should be killed after detach.
    #[serde(default)]
    pub kill_on_detach: bool,
    /// Optional client-local shell command to run before detaching.
    #[serde(default)]
    pub exec_command: Option<String>,
}

/// Request payload for `refresh-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshClientRequest {
    /// The optional target-client identifier or `=`.
    #[serde(default)]
    pub target_client: Option<String>,
    /// Reserved wire-v5 field for a client-pan adjustment.
    ///
    /// RMUX 0.9 does not expose client panning and rejects a populated value
    /// fail-closed when received from an older client.
    #[serde(default)]
    pub adjustment: Option<u32>,
    /// Reserved wire-v5 field for clearing client panning.
    #[serde(default)]
    pub clear_pan: bool,
    /// Reserved wire-v5 field for panning the client view left.
    #[serde(default)]
    pub pan_left: bool,
    /// Reserved wire-v5 field for panning the client view right.
    #[serde(default)]
    pub pan_right: bool,
    /// Reserved wire-v5 field for panning the client view up.
    #[serde(default)]
    pub pan_up: bool,
    /// Reserved wire-v5 field for panning the client view down.
    #[serde(default)]
    pub pan_down: bool,
    /// Whether only the status line should be redrawn.
    #[serde(default)]
    pub status_only: bool,
    /// Whether the client clipboard should be queried.
    #[serde(default)]
    pub clipboard_query: bool,
    /// Optional client-flag string from `-f`.
    #[serde(default)]
    pub flags: Option<String>,
    /// Optional client-flag string from `-F`, which tmux treats as an alias for `-f`.
    #[serde(default)]
    pub flags_alias: Option<String>,
    /// Reserved wire-v5 field for control-mode subscription updates.
    ///
    /// RMUX 0.9 does not expose `refresh-client -A` and rejects non-empty
    /// values fail-closed when received from an older client.
    #[serde(default)]
    pub subscriptions: Vec<String>,
    /// Reserved wire-v5 field for control-mode subscription definitions.
    ///
    /// RMUX 0.9 does not expose `refresh-client -B` and rejects non-empty
    /// values fail-closed when received from an older client.
    #[serde(default)]
    pub subscriptions_format: Vec<String>,
    /// Optional control-mode size string from `-C`.
    #[serde(default)]
    pub control_size: Option<String>,
    /// Reserved wire-v5 field for control-mode colour reports.
    ///
    /// RMUX 0.9 does not expose `refresh-client -r` and rejects a populated
    /// value fail-closed when received from an older client.
    #[serde(default)]
    pub colour_report: Option<String>,
}

/// Request payload for `list-clients`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListClientsRequest {
    /// Optional custom output format.
    #[serde(default)]
    pub format: Option<String>,
    /// Optional filter expression.
    #[serde(default)]
    pub filter: Option<String>,
    /// Optional sort-order token.
    #[serde(default)]
    pub sort_order: Option<String>,
    /// Whether the listing should be reversed.
    #[serde(default)]
    pub reversed: bool,
    /// Optional session filter.
    #[serde(default)]
    pub target_session: Option<SessionName>,
}

/// Request payload for `suspend-client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspendClientRequest {
    /// The optional target-client identifier or `=`.
    #[serde(default)]
    pub target_client: Option<String>,
}
