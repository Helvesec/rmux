#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Pure in-memory RMUX domain model state.
//!
//! This crate models sessions, windows, panes, layout geometry, and exact
//! target resolution without any OS, network, or process integration.

mod box_lines;
mod buffers;
pub mod command_parser;
pub mod command_queue;
mod environment;
pub mod formats;
mod glob;
mod grid;
mod hooks;
mod hyperlinks;
/// tmux-compatible VT parser state machine, CSI/ESC/OSC dispatch, and SGR.
pub mod input;
mod keys;
mod layout;
mod lifecycle;
mod options;
mod pane;
mod screen;
mod session;
pub mod style;
mod target;
mod target_find;
mod terminal_sequences;
mod transcript;
mod utf8;
mod vis;
mod window;

pub use box_lines::BoxLines;
pub use buffers::{BufferStore, BufferView, RenameBufferOutcome, SetBufferOutcome};
pub use environment::{EnvironmentStore, ShowEnvironmentEntry, ENVIRON_HIDDEN};
pub use formats::format_skip_delimiter;
pub use grid::GridRenderOptions;
pub use hooks::{
    validate_hook_registration, validate_hook_scope, HookBindingView, HookDispatch, HookGlobalRoot,
    HookSetOptions, HookStore,
};
pub use input::{
    colour_join_rgb, Colour, GridAttr, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB,
    COLOUR_NONE, COLOUR_TERMINAL,
};
pub use keys::{
    key_code_is_mouse_move, key_code_lookup_bits, key_code_to_bytes, key_string_lookup_key,
    key_string_lookup_string, parse_binding_command_tokens, KeyBinding, KeyBindingDisplay,
    KeyBindingSortOrder, KeyBindingStore, KeyBindingTable, KeyBindingTableRef, KeyCode, KEYC_ANY,
    KEYC_BSPACE, KEYC_BUILD_MODIFIERS, KEYC_CTRL, KEYC_CURSOR, KEYC_DRAGGING, KEYC_IMPLIED_META,
    KEYC_KEYPAD, KEYC_LITERAL, KEYC_MASK_FLAGS, KEYC_MASK_KEY, KEYC_MASK_MODIFIERS, KEYC_MASK_TYPE,
    KEYC_META, KEYC_NONE, KEYC_SENT, KEYC_SHIFT, KEYC_UNKNOWN, KEYC_USER, KEYC_VI,
    LIST_KEYS_TEMPLATE,
};
pub use lifecycle::LifecycleEvent;
pub use options::{
    option_affects_alerts, option_affects_rendering, option_name_by_name, resolve_option_name,
    validate_option_mutation, validate_option_name_mutation, OptionMutationOutcome,
    OptionNotification, OptionStore, ShowOptionsMode,
};
pub use pane::{Pane, PaneGeometry, PaneId};
pub use screen::{Screen, ScreenCellView, ScreenLineView};
pub use session::{
    BreakPaneOptions, KillPaneOutcome, PaneJoinOptions, PaneSwapOptions, Session,
    SessionPaneTarget, SessionStore,
};
pub use style::{
    colour_to_string, parse_colour, style_parse, style_tostring, ColourParseError, Style,
    StyleAlign, StyleCell, StyleDefaultType, StyleList, StyleParseError, StyleRange, StyleWidth,
};
pub use target_find::{
    command_target_metadata, CommandTargetMetadata, CommandTargetSpec, TargetFindContext,
    TargetFindFlags, TargetFindType, UnresolvedTarget,
};
pub use terminal_sequences::{alternate_screen_enter_sequence, alternate_screen_exit_sequence};
pub use transcript::{ScreenCaptureRange, Transcript};
pub use utf8::{text_width, truncate_to_width, Utf8Config};
pub use vis::encode_paste_bytes;
pub use window::{
    AlertFlags, Window, WINDOW_ACTIVITY, WINDOW_ALERTFLAGS, WINDOW_BELL, WINDOW_SILENCE,
    WINLINK_ACTIVITY, WINLINK_ALERTFLAGS, WINLINK_BELL, WINLINK_SILENCE,
};

/// Returns whether `text` matches the tmux-style glob `pattern`.
#[must_use]
pub fn fnmatch(pattern: &str, text: &str) -> bool {
    glob::fnmatch(pattern, text)
}
