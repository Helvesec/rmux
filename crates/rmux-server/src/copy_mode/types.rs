use std::path::PathBuf;

use rmux_core::Screen;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeKeys {
    Emacs,
    Vi,
}

impl ModeKeys {
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or_default() {
            "vi" => Self::Vi,
            _ => Self::Emacs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeCommandContext {
    pub(crate) mode_keys: ModeKeys,
    pub(crate) wrap_search: bool,
    pub(crate) word_separators: String,
    pub(crate) default_shell: String,
    pub(crate) working_directory: Option<PathBuf>,
    pub(crate) refresh_screen: Option<Screen>,
    pub(crate) mouse: Option<CopyModeMouseContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyModeMouseContext {
    pub(crate) content_x: u32,
    pub(crate) content_y: u16,
    pub(crate) scroll_y: u16,
    pub(crate) slider_mpos: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeSummary {
    pub(crate) view_mode: bool,
    pub(crate) scroll_position: usize,
    pub(crate) rectangle_toggle: bool,
    pub(crate) cursor_x: u32,
    pub(crate) cursor_y: usize,
    pub(crate) selection_start: Option<CopyPosition>,
    pub(crate) selection_end: Option<CopyPosition>,
    pub(crate) selection_active: bool,
    pub(crate) selection_present: bool,
    pub(crate) selection_mode: Option<SelectionMode>,
    pub(crate) search_present: bool,
    pub(crate) search_timed_out: bool,
    pub(crate) search_count: usize,
    pub(crate) search_count_partial: bool,
    pub(crate) search_match: Option<String>,
    pub(crate) copy_cursor_word: String,
    pub(crate) copy_cursor_line: String,
    pub(crate) copy_cursor_hyperlink: String,
    pub(crate) pane_search_string: String,
    pub(crate) top_line_time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeCommandOutcome {
    pub(crate) cancel: bool,
    pub(crate) transfer: Option<CopyModeTransfer>,
}

impl CopyModeCommandOutcome {
    pub(crate) fn nothing() -> Self {
        Self {
            cancel: false,
            transfer: None,
        }
    }

    pub(super) fn cancel() -> Self {
        Self {
            cancel: true,
            transfer: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeTransfer {
    pub(crate) data: Vec<u8>,
    pub(crate) buffer_target: Option<CopyBufferTarget>,
    pub(crate) append: bool,
    pub(crate) pipe_command: Option<CopyModePipeCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyModePipeCommand {
    Explicit(String),
    CopyCommandOption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyBufferTarget {
    New(Option<String>),
    Top,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionMode {
    Char,
    Word,
    Line,
}

impl SelectionMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "char" | "c" => Some(Self::Char),
            "word" | "w" => Some(Self::Word),
            "line" | "l" => Some(Self::Line),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Char => "char",
            Self::Word => "word",
            Self::Line => "line",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyPosition {
    pub(crate) x: u32,
    pub(crate) y: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClearPolicy {
    Always,
    Never,
    EmacsOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpKind {
    Forward,
    Backward,
    ToForward,
    ToBackward,
}

impl JumpKind {
    pub(super) fn reverse(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
            Self::ToForward => Self::ToBackward,
            Self::ToBackward => Self::ToForward,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JumpState {
    pub(super) kind: JumpKind,
    pub(super) ch: char,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelectionState {
    pub(super) anchor: CopyPosition,
    pub(super) end: CopyPosition,
    pub(super) mode: SelectionMode,
    pub(super) active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchMatch {
    pub(super) start: CopyPosition,
    pub(super) end: CopyPosition,
    pub(super) text: String,
}
