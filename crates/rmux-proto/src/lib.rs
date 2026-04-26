#![deny(missing_docs)]
#![forbid(unsafe_code)]

//! Shared detached protocol types for RMUX.

pub mod attach;
pub mod codec;
pub mod control;
pub mod envelope;
pub mod error;
pub mod request;
pub mod response;
pub mod types;

pub use attach::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke, KeyDispatched,
};
pub use codec::{decode_frame, encode_frame, FrameDecoder, DEFAULT_MAX_FRAME_LENGTH};
pub use control::{
    format_continue_line, format_exit_line, format_extended_output_line, format_guard_line,
    format_output_line, format_pause_line, octal_escape, ClientTerminalContext, ControlGuardKind,
    ControlMode, ControlModeRequest, ControlModeResponse, CONTROL_BUFFER_HIGH, CONTROL_BUFFER_LOW,
    CONTROL_CONTROL_END, CONTROL_CONTROL_START, CONTROL_MAXIMUM_AGE_MS, CONTROL_WRITE_MINIMUM,
};
pub use envelope::{RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION};
pub use error::RmuxError;
pub use request::*;
pub use response::*;
pub use types::OptionScopeSelector;
pub use types::*;

/// Detached request/response protocol revision.
pub const PROTOCOL_VERSION: u16 = RMUX_WIRE_VERSION as u16;
