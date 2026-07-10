use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::PaneTarget;

/// Request payload for `set-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetBufferRequest {
    /// The optional buffer name. When `None`, an unnamed buffer is created.
    pub name: Option<String>,
    /// The buffer content.
    pub content: Vec<u8>,
    /// Whether new content should append to an existing buffer.
    #[serde(default)]
    pub append: bool,
    /// Optional new name for a rename-only mutation.
    #[serde(default)]
    pub new_name: Option<String>,
    /// Whether the buffer should also be copied to the client clipboard.
    #[serde(default)]
    pub set_clipboard: bool,
    /// Optional attached client whose clipboard should receive `-w` output.
    #[serde(default)]
    pub target_client: Option<String>,
}

/// Request payload for `show-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowBufferRequest {
    /// The optional buffer name. When `None`, the stack-head buffer is shown.
    pub name: Option<String>,
}

/// Request payload for `paste-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteBufferRequest {
    /// The optional buffer name. When `None`, the stack-head buffer is pasted.
    pub name: Option<String>,
    /// The target pane to write the buffer content to.
    pub target: PaneTarget,
    /// Whether to delete the buffer after pasting.
    pub delete_after: bool,
    /// Optional replacement separator for embedded newlines.
    #[serde(default)]
    pub separator: Option<String>,
    /// Whether newline separators should use `\\n` instead of `\\r`.
    #[serde(default)]
    pub linefeed: bool,
    /// Whether raw bytes should be written without vis-style escaping.
    #[serde(default)]
    pub raw: bool,
    /// Whether bracketed paste wrappers should be emitted when enabled on the pane.
    #[serde(default)]
    pub bracketed: bool,
}

/// Request payload for `list-buffers`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListBuffersRequest {
    /// Optional format template.
    #[serde(default)]
    pub format: Option<String>,
    /// Optional filter expression.
    #[serde(default)]
    pub filter: Option<String>,
    /// Optional sort order string.
    #[serde(default)]
    pub sort_order: Option<String>,
    /// Whether to reverse the rendered order.
    #[serde(default)]
    pub reversed: bool,
}

/// Request payload for `delete-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteBufferRequest {
    /// The optional buffer name. When `None`, the stack-head buffer is deleted.
    pub name: Option<String>,
}

/// Request payload for `load-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadBufferRequest {
    /// The caller-supplied path to read.
    pub path: String,
    /// The caller working directory used to resolve relative paths.
    pub cwd: Option<PathBuf>,
    /// The optional buffer name. When `None`, an unnamed buffer is created.
    pub name: Option<String>,
    /// Whether the loaded content should also be copied to the client clipboard.
    #[serde(default)]
    pub set_clipboard: bool,
    /// Optional attached client whose clipboard should receive `-w` output.
    #[serde(default)]
    pub target_client: Option<String>,
}

/// Request payload for `save-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveBufferRequest {
    /// The caller-supplied path to write.
    pub path: String,
    /// The caller working directory used to resolve relative paths.
    pub cwd: Option<PathBuf>,
    /// The optional buffer name. When `None`, the stack-head buffer is saved.
    pub name: Option<String>,
    /// Whether output should append to the file instead of replacing it.
    #[serde(default)]
    pub append: bool,
}

/// Request payload for `capture-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturePaneRequest {
    /// The pane whose retained transcript should be captured.
    pub target: PaneTarget,
    /// The optional inclusive start line.
    pub start: Option<i64>,
    /// The optional inclusive end line.
    pub end: Option<i64>,
    /// Whether to print captured bytes to stdout instead of writing a buffer.
    pub print: bool,
    /// The optional destination buffer name for non-printing captures.
    pub buffer_name: Option<String>,
    /// Whether the saved alternate-screen copy should be captured.
    #[serde(default)]
    pub alternate: bool,
    /// Whether ANSI SGR and hyperlink sequences should be preserved.
    #[serde(default)]
    pub escape_ansi: bool,
    /// Whether control sequences should be octal-escaped.
    #[serde(default)]
    pub escape_sequences: bool,
    /// Whether each captured line should be prefixed with tmux line flags.
    #[serde(default)]
    pub include_format: bool,
    /// Whether hyperlink information should be captured instead of text.
    #[serde(default)]
    pub hyperlinks: bool,
    /// Whether each captured line should be prefixed with its line number.
    #[serde(default)]
    pub line_numbers: bool,
    /// Whether wrapped rows should be joined without intervening newlines.
    #[serde(default)]
    pub join_wrapped: bool,
    /// Whether the copy-mode screen should be captured when present.
    #[serde(default)]
    pub use_mode_screen: bool,
    /// Whether trailing spaces should be preserved.
    #[serde(default)]
    pub preserve_trailing_spaces: bool,
    /// Whether trailing spaces should not be trimmed.
    #[serde(default)]
    pub do_not_trim_spaces: bool,
    /// Whether pending parser bytes should be captured instead of the screen grid.
    #[serde(default)]
    pub pending_input: bool,
    /// Whether missing alternate-screen content should be silenced.
    #[serde(default)]
    pub quiet: bool,
    /// Whether `-S -` was used.
    #[serde(default)]
    pub start_is_absolute: bool,
    /// Whether `-E -` was used.
    #[serde(default)]
    pub end_is_absolute: bool,
}

/// Request payload for `capture-pane` carrying raw tmux target text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturePaneTargetActionRequest {
    /// Optional raw `-t` target. `None` uses the requester's current pane.
    #[serde(default)]
    pub target: Option<String>,
    /// The optional inclusive start line.
    pub start: Option<i64>,
    /// The optional inclusive end line.
    pub end: Option<i64>,
    /// Whether to print captured bytes to stdout instead of writing a buffer.
    pub print: bool,
    /// The optional destination buffer name for non-printing captures.
    pub buffer_name: Option<String>,
    /// Whether the saved alternate-screen copy should be captured.
    #[serde(default)]
    pub alternate: bool,
    /// Whether ANSI SGR and hyperlink sequences should be preserved.
    #[serde(default)]
    pub escape_ansi: bool,
    /// Whether control sequences should be octal-escaped.
    #[serde(default)]
    pub escape_sequences: bool,
    /// Whether each captured line should be prefixed with tmux line flags.
    #[serde(default)]
    pub include_format: bool,
    /// Whether hyperlink information should be captured instead of text.
    #[serde(default)]
    pub hyperlinks: bool,
    /// Whether each captured line should be prefixed with its line number.
    #[serde(default)]
    pub line_numbers: bool,
    /// Whether wrapped rows should be joined without intervening newlines.
    #[serde(default)]
    pub join_wrapped: bool,
    /// Whether the copy-mode screen should be captured when present.
    #[serde(default)]
    pub use_mode_screen: bool,
    /// Whether trailing spaces should be preserved.
    #[serde(default)]
    pub preserve_trailing_spaces: bool,
    /// Whether trailing spaces should not be trimmed.
    #[serde(default)]
    pub do_not_trim_spaces: bool,
    /// Whether pending parser bytes should be captured instead of the screen grid.
    #[serde(default)]
    pub pending_input: bool,
    /// Whether missing alternate-screen content should be silenced.
    #[serde(default)]
    pub quiet: bool,
    /// Whether `-S -` was used.
    #[serde(default)]
    pub start_is_absolute: bool,
    /// Whether `-E -` was used.
    #[serde(default)]
    pub end_is_absolute: bool,
}

/// Request payload for `clear-history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearHistoryRequest {
    /// The target pane whose history should be cleared.
    pub target: PaneTarget,
    /// Whether OSC 8 hyperlink storage should also be reset.
    #[serde(default)]
    pub reset_hyperlinks: bool,
}
