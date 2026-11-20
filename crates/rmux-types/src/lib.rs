#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Portable semantic newtypes shared by non-adjacent RMUX crates.

pub mod identity;
pub use identity::{PaneId, SessionId, SessionName, SessionNameError, WindowId};

/// A terminal geometry request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalSize {
    /// The requested column count.
    pub cols: u16,
    /// The requested row count.
    pub rows: u16,
}

impl TerminalSize {
    /// Creates a terminal size value from column and row counts.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// Returns this cell geometry wrapped as a terminal geometry with no pixel size.
    #[must_use]
    pub const fn into_geometry(self) -> TerminalGeometry {
        TerminalGeometry::from_size(self)
    }
}

/// Terminal pixel dimensions reported by terminals that expose `TIOCGWINSZ`
/// pixel fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalPixels {
    /// The terminal width in pixels.
    pub width: u16,
    /// The terminal height in pixels.
    pub height: u16,
}

impl TerminalPixels {
    /// Creates terminal pixel dimensions.
    #[must_use]
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

/// A terminal geometry request including cell dimensions and optional pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalGeometry {
    /// The terminal size in character cells.
    pub size: TerminalSize,
    /// The terminal size in pixels, when the outer terminal exposes it.
    pub pixels: Option<TerminalPixels>,
}

impl TerminalGeometry {
    /// Creates terminal geometry from cell dimensions.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self {
            size: TerminalSize::new(cols, rows),
            pixels: None,
        }
    }

    /// Creates terminal geometry from an existing cell size.
    #[must_use]
    pub const fn from_size(size: TerminalSize) -> Self {
        Self { size, pixels: None }
    }

    /// Adds pixel dimensions to this geometry.
    #[must_use]
    pub const fn with_pixels(mut self, pixels: TerminalPixels) -> Self {
        self.pixels = Some(pixels);
        self
    }
}
