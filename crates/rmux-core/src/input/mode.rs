//! Mode flag bits matching tmux `tmux.h:660-680`.

/// Cursor visible.
pub const MODE_CURSOR: u32 = 0x1;
/// Insert mode.
pub const MODE_INSERT: u32 = 0x2;
/// Application cursor keys.
pub const MODE_KCURSOR: u32 = 0x4;
/// Application keypad.
pub const MODE_KKEYPAD: u32 = 0x8;
/// Auto wrap.
pub const MODE_WRAP: u32 = 0x10;
/// Standard mouse reporting (1000).
pub const MODE_MOUSE_STANDARD: u32 = 0x20;
/// Button-event mouse tracking (1002).
pub const MODE_MOUSE_BUTTON: u32 = 0x40;
/// Cursor blinking.
pub const MODE_CURSOR_BLINKING: u32 = 0x80;
/// Mouse UTF-8 mode (1005).
pub const MODE_MOUSE_UTF8: u32 = 0x100;
/// SGR mouse mode (1006).
pub const MODE_MOUSE_SGR: u32 = 0x200;
/// Bracketed paste.
pub const MODE_BRACKETPASTE: u32 = 0x400;
/// Focus in/out events.
pub const MODE_FOCUSON: u32 = 0x800;
/// All mouse tracking (1003).
pub const MODE_MOUSE_ALL: u32 = 0x1000;
/// Origin mode.
pub const MODE_ORIGIN: u32 = 0x2000;
/// CR+LF mode.
pub const MODE_CRLF: u32 = 0x4000;
/// Extended keys mode.
pub const MODE_KEYS_EXTENDED: u32 = 0x8000;
/// Cursor very visible (blinking block, from DECTCEM handling).
pub const MODE_CURSOR_VERY_VISIBLE: u32 = 0x1_0000;
/// Cursor blinking explicitly set.
pub const MODE_CURSOR_BLINKING_SET: u32 = 0x2_0000;
/// Extended keys mode 2.
pub const MODE_KEYS_EXTENDED_2: u32 = 0x4_0000;
/// Theme updates from application.
pub const MODE_THEME_UPDATES: u32 = 0x8_0000;
/// Synchronized output.
pub const MODE_SYNC: u32 = 0x10_0000;
/// Kitty keyboard protocol, encoded with CSI-u sequences.
pub const MODE_KEYS_KITTY: u32 = 0x20_0000;

/// All mouse modes combined.
pub const ALL_MOUSE_MODES: u32 = MODE_MOUSE_STANDARD | MODE_MOUSE_BUTTON | MODE_MOUSE_ALL;
/// Extended key modes combined.
pub const EXTENDED_KEY_MODES: u32 = MODE_KEYS_EXTENDED | MODE_KEYS_EXTENDED_2 | MODE_KEYS_KITTY;
