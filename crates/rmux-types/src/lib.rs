#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Portable semantic newtypes shared by non-adjacent RMUX crates.

/// A terminal geometry request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalSize {
    /// The requested column count.
    pub cols: u16,
    /// The requested row count.
    pub rows: u16,
}
