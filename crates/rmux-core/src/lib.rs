#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Pure in-memory RMUX domain model state.
//!
//! This crate models sessions, windows, panes, layout geometry, and exact
//! target resolution without any OS, network, or process integration.

/// tmux-compatible message for invalid `-O` sort-order arguments.
pub const INVALID_SORT_ORDER: &str = "invalid sort order";

mod box_lines;
mod buffers;
/// Shared tmux-compatible `list-commands` inventory and renderer.
pub mod command_inventory;
pub mod command_parser;
pub mod command_queue;
mod dec_modes;
mod environment;
/// Bounded event buffers and cursor accounting.
pub mod events;
pub mod formats;
mod glob;
mod grid;
mod hooks;
mod hyperlinks;
pub mod identity;
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
mod terminal;
mod terminal_passthrough;
mod terminal_screen;
mod terminal_sequences;
pub mod tmux_precedence;
mod transcript;
mod utf8;
mod vis;
mod window;

pub use box_lines::BoxLines;
pub use buffers::{BufferStore, BufferView, RenameBufferOutcome, SetBufferOutcome};
pub use dec_modes::{render_dec_modes, render_dec_modes_for_snapshot};
pub use environment::{EnvironmentStore, ShowEnvironmentEntry, ENVIRON_HIDDEN};
pub use formats::format_skip_delimiter;
pub use grid::GridRenderOptions;
pub use hooks::{
    hook_explicit_scope_for_target, hook_global_root, hook_natural_scope_for_session_target,
    hook_natural_scope_for_target, validate_hook_registration, validate_hook_scope,
    HookBindingView, HookDispatch, HookGlobalRoot, HookScopeIdentity, HookSetOptions, HookStore,
};
pub use identity::{PaneId, SessionId, SessionName, WindowId};
pub use input::{
    colour_join_rgb, Colour, GridAttr, COLOUR_DEFAULT, COLOUR_FLAG_256, COLOUR_FLAG_RGB,
    COLOUR_NONE, COLOUR_TERMINAL,
};
pub use keys::{
    key_code_is_mouse_move, key_code_lookup_bits, key_code_to_bytes, key_string_lookup_key,
    key_string_lookup_string, parse_binding_command_tokens,
    parse_binding_command_tokens_with_parser, KeyBinding, KeyBindingDisplay, KeyBindingSortOrder,
    KeyBindingStore, KeyBindingTable, KeyBindingTableRef, KeyCode, KEYC_ANY, KEYC_BSPACE,
    KEYC_BUILD_MODIFIERS, KEYC_CTRL, KEYC_CURSOR, KEYC_DRAGGING, KEYC_IMPLIED_META, KEYC_KEYPAD,
    KEYC_LITERAL, KEYC_MASK_FLAGS, KEYC_MASK_KEY, KEYC_MASK_MODIFIERS, KEYC_MASK_TYPE, KEYC_META,
    KEYC_NONE, KEYC_SENT, KEYC_SHIFT, KEYC_UNKNOWN, KEYC_USER, KEYC_VI, LIST_KEYS_TEMPLATE,
};
pub use lifecycle::LifecycleEvent;
pub use options::{
    default_global_scope_for_option_name, option_affects_alerts, option_affects_rendering,
    option_name_by_name, resolve_option_name, resolve_option_name_typed, validate_option_mutation,
    validate_option_name_mutation, OptionLookupError, OptionMutationOutcome, OptionNotification,
    OptionStore, ShowOptionsMode,
};
pub use pane::{Pane, PaneGeometry};
pub use screen::{Screen, ScreenCellRef, ScreenCellView, ScreenLineView};
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
pub use terminal_passthrough::{
    TerminalPaletteIndex, TerminalPassthrough, TerminalPassthroughKind,
};
pub use terminal_screen::TerminalScreen;
pub use terminal_sequences::{alternate_screen_enter_sequence, alternate_screen_exit_sequence};
pub use transcript::{ScreenCaptureRange, Transcript};
pub use utf8::{text_width, truncate_right_to_width, truncate_to_width, Utf8Config};
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
