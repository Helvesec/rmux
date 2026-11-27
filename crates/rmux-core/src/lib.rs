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
pub mod cli_entry;
/// Shared tmux-compatible `list-commands` inventory and renderer.
pub mod command_inventory;
pub mod command_parser;
pub mod command_queue;
mod command_schema;
mod cursor_style;
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
/// Portable Kitty keyboard event, codec, and per-screen state primitives.
pub mod kitty_keyboard;
mod layout;
mod lifecycle;
mod options;
mod pane;
mod program_status;
mod recovery_row;
mod resolved_scene;
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
mod window_state_store;

pub use box_lines::BoxLines;
pub use buffers::{BufferStore, BufferView, RenameBufferOutcome, SetBufferOutcome};
pub use cursor_style::{
    cursor_style_from_option, cursor_style_is_valid, resolve_cursor_style,
    resolve_cursor_style_state, ResolvedCursorStyle,
};
pub use dec_modes::{render_dec_modes, render_dec_modes_for_snapshot};
pub use environment::{EnvironmentStore, ShowEnvironmentEntry, ENVIRON_HIDDEN};
pub use formats::format_skip_delimiter;
pub use grid::{GridRenderOptions, RenderedLineSpan};
pub use hooks::{
    hook_explicit_scope_for_target, hook_global_root, hook_natural_scope_for_session_target,
    hook_natural_scope_for_target, validate_hook_registration, validate_hook_scope,
    HookBindingView, HookDispatch, HookGlobalRoot, HookScopeIdentity, HookSetOptions, HookStore,
};
pub use identity::{PaneId, SessionId, SessionName, WindowId};
pub use input::{
    colour_join_rgb, colour_to_rgb, Colour, GridAttr, COLOUR_DEFAULT, COLOUR_FLAG_256,
    COLOUR_FLAG_RGB, COLOUR_NONE, COLOUR_TERMINAL,
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
pub use program_status::ProgramStatus;
pub use resolved_scene::{
    ResolvedHitRegion, ResolvedHitRegionKind, ResolvedPaneBorderStatus, ResolvedPaneLayer,
    ResolvedPaneScene, ResolvedScene, ResolvedSceneContext, ResolvedZoomVisibility, SignedPaneRect,
};
pub use screen::{
    LineFormatCapture, RecoveryRow, RecoveryRowRenderer, Screen, ScreenCellRef, ScreenCellView,
    ScreenLineView,
};
pub use session::{
    AttachedWindowGeometry, BreakPaneOptions, KillPaneOutcome, ManagedGroupNavigation,
    ManagedIndexChange, ManagedPaneBreak, ManagedPaneCreation, ManagedPaneRemoval,
    ManagedPaneSplit, ManagedPaneTransfer, ManagedSessionIndexChange, ManagedSessionPresentation,
    ManagedSessionRemoval, ManagedSessionStore, ManagedSessionView, ManagedStructureCheckpoint,
    ManagedWindowCheckpoint, ManagedWindowCreation, ManagedWindowNameChange, ManagedWindowRemoval,
    PaneJoinOptions, PaneSwapOptions, Session, SessionPaneTarget, SessionRecency, SessionStore,
    WindowLayoutContext, WindowSelection, WindowSwitch,
};
pub use style::{
    colour_to_string, parse_colour, style_parse, style_tostring, ColourParseError, Style,
    StyleAlign, StyleCell, StyleDefaultType, StyleList, StyleParseError, StyleRange, StyleWidth,
};
pub use target_find::{
    command_target_metadata, CommandTargetMetadata, CommandTargetSpec,
    MissingCurrentTargetFallback, TargetFindContext, TargetFindFlags, TargetFindType,
    UnresolvedTarget,
};
pub use terminal_passthrough::{
    TerminalClipboardQuery, TerminalPaletteIndex, TerminalPassthrough, TerminalPassthroughKind,
    SIXEL_SUPPORT,
};
pub use terminal_screen::TerminalScreen;
pub use terminal_sequences::{alternate_screen_enter_sequence, alternate_screen_exit_sequence};
pub use transcript::{parse_capture_bound, ScreenCaptureRange, Transcript};
pub use utf8::{text_width, truncate_right_to_width, truncate_to_width, Utf8Config};
pub use vis::encode_paste_bytes;
pub use window::{
    cap_window_size, parse_window_dimension, validate_explicit_window_size, AlertFlags,
    FloatingPanePlacement, FloatingPaneRect, FloatingPaneRequest, FloatingStack, ManualWindowSize,
    Window, WindowPaneActivity, WINDOW_ACTIVITY, WINDOW_ALERTFLAGS, WINDOW_BELL, WINDOW_MAXIMUM,
    WINDOW_MINIMUM, WINDOW_SILENCE, WINLINK_ACTIVITY, WINLINK_ALERTFLAGS, WINLINK_BELL,
    WINLINK_SILENCE,
};

pub use window_state_store::{
    IntroduceWindowError, WindowLink, WindowStateError, WindowStateStore,
};

/// Returns whether `text` matches the tmux-style glob `pattern`.
#[must_use]
pub fn fnmatch(pattern: &str, text: &str) -> bool {
    glob::fnmatch(pattern, text)
}

#[cfg(any(test, feature = "test-seams"))]
pub use session::ManagedSessionTestSnapshot;
