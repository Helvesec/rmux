#![deny(missing_docs)]
#![forbid(unsafe_code)]

//! Shared detached protocol types for RMUX.

pub mod attach;
pub mod capabilities;
pub mod codec;
pub mod control;
pub mod envelope;
pub mod error;
pub mod frame_kind;
pub mod identity;
pub mod request;
pub mod response;
pub mod types;

pub use attach::{
    decode_attach_data_frame, decode_attach_data_frame_with_limit, encode_attach_data,
    encode_attach_data_into_slice, encode_attach_message, AttachDataFrame, AttachFrameDecoder,
    AttachMessage, AttachShellCommand, AttachedKeystroke, AttachedWindowsConsoleKey, KeyDispatched,
    ATTACH_DATA_HEADER_LEN,
};
pub use capabilities::{
    HandshakeRequest, HandshakeResponse, CAPABILITY_ATTACH_RENDER,
    CAPABILITY_ATTACH_RESIZE_GEOMETRY, CAPABILITY_ATTACH_STREAM,
    CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY, CAPABILITY_CLI_CAPTURE_TARGET_ACTION,
    CAPABILITY_CLI_TARGET_ACTIONS, CAPABILITY_CONTROL_STREAM, CAPABILITY_DAEMON_SHUTDOWN,
    CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE, CAPABILITY_DAEMON_STATUS, CAPABILITY_DETACHED_RPC,
    CAPABILITY_FRAMED_ERRORS, CAPABILITY_HANDSHAKE, CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY,
    CAPABILITY_SDK_PANE_BROADCAST, CAPABILITY_SDK_PANE_BY_ID, CAPABILITY_SDK_PANE_FOREGROUND,
    CAPABILITY_SDK_PANE_OPTIONS, CAPABILITY_SDK_PANE_STATE_EVENTS, CAPABILITY_SDK_PROCESS_COMMAND,
    CAPABILITY_SDK_SESSION_LEASE, CAPABILITY_SDK_SESSION_LEASE_BY_ID,
    CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2, CAPABILITY_SDK_WAITS, CAPABILITY_SDK_WAITS_ARMED,
    CAPABILITY_TARGET_CLIENT_COMMANDS, CAPABILITY_WEB_SHARE, SUPPORTED_CAPABILITIES,
};
#[cfg(feature = "fuzzing")]
pub use codec::fuzz_detached_frame_decoder;
pub use codec::{
    decode_frame, encode_frame, FrameDecoder, DEFAULT_MAX_DETACHED_FRAME_LENGTH,
    DEFAULT_MAX_FRAME_LENGTH,
};
pub use control::{
    format_continue_line, format_exit_line, format_extended_output_line, format_guard_line,
    format_output_line, format_pause_line, octal_escape, ClientTerminalContext, ControlGuardKind,
    ControlMode, ControlModeRequest, ControlModeResponse, CONTROL_BUFFER_HIGH, CONTROL_BUFFER_LOW,
    CONTROL_CONTROL_END, CONTROL_CONTROL_START, CONTROL_MAXIMUM_AGE_MS, CONTROL_STDIN_EOF_MARKER,
    CONTROL_WRITE_MINIMUM,
};
pub use envelope::{RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION};
pub use error::{
    RmuxError, OWNED_SESSION_LEASE_LOST_MESSAGE_PREFIX, PANE_STILL_ACTIVE_MESSAGE,
    PROCESS_COMMAND_EMPTY_MESSAGE, SPAWN_FAILED_MESSAGE_PREFIX,
};
pub use frame_kind::{
    frame_kind_for_request, frame_kind_for_response, ledger_entry_for, FrameDirection,
    FrameFeature, FrameKind, FrameLedgerEntry, FrameStatus, V1_FRAME_LEDGER,
};
pub use identity::{PaneId, SessionId, SessionName, WindowId};
pub use request::*;
pub use response::*;
pub use types::*;
pub use types::{
    OptionScopeSelector, PaneOutputSubscriptionId, PaneStateSubscriptionId, SdkWaitId,
    SdkWaitOwnerId,
};

/// Detached request/response protocol revision.
pub const PROTOCOL_VERSION: u16 = RMUX_WIRE_VERSION as u16;

/// Minimum daemon-side TTL accepted for owned-session leases.
pub const MIN_SESSION_LEASE_TTL_MILLIS: u64 = 500;
