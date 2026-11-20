//! Layout names, split orientation and pane-resize adjustments.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::RmuxError;

/// The detached layout name subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutName {
    /// The required `main-vertical` layout.
    MainVertical,
    /// Internal `split-window -h` geometry using a top main pane.
    MainHorizontal,
    /// The tmux `even-horizontal` left-to-right layout.
    EvenHorizontal,
    /// The tmux `even-vertical` top-to-bottom layout.
    EvenVertical,
    /// The tmux `tiled` grid layout.
    Tiled,
    /// The tmux `main-horizontal-mirrored` layout.
    MainHorizontalMirrored,
    /// The tmux `main-vertical-mirrored` layout.
    MainVerticalMirrored,
}

impl fmt::Display for LayoutName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MainVertical => formatter.write_str("main-vertical"),
            Self::MainHorizontal => formatter.write_str("main-horizontal"),
            Self::EvenHorizontal => formatter.write_str("even-horizontal"),
            Self::EvenVertical => formatter.write_str("even-vertical"),
            Self::Tiled => formatter.write_str("tiled"),
            Self::MainHorizontalMirrored => formatter.write_str("main-horizontal-mirrored"),
            Self::MainVerticalMirrored => formatter.write_str("main-vertical-mirrored"),
        }
    }
}

impl FromStr for LayoutName {
    type Err = RmuxError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "main-vertical" => Ok(Self::MainVertical),
            "main-horizontal" => Ok(Self::MainHorizontal),
            "even-horizontal" => Ok(Self::EvenHorizontal),
            "even-vertical" => Ok(Self::EvenVertical),
            "tiled" => Ok(Self::Tiled),
            "main-horizontal-mirrored" => Ok(Self::MainHorizontalMirrored),
            "main-vertical-mirrored" => Ok(Self::MainVerticalMirrored),
            _ => Err(RmuxError::Server(format!("unknown layout: {value}"))),
        }
    }
}

/// Wire-level split orientation accepted by `split-window`.
///
/// The variant names follow tmux's flag convention (pane arrangement), not
/// the divider-line convention: `Horizontal` means "panes arranged
/// horizontally" (side by side), `Vertical` means "panes arranged
/// vertically" (stacked). New SDK code should prefer
/// [`rmux_sdk::SplitDirection`](https://docs.rs/rmux-sdk/latest/rmux_sdk/enum.SplitDirection.html)
/// (`Right`/`Left`/`Up`/`Down`), which avoids this ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SplitDirection {
    /// Stacked panes (top + bottom). Matches tmux `split-window -v`,
    /// the tmux default when no flag is passed.
    #[default]
    Vertical,
    /// Side-by-side panes (left + right). Matches tmux `split-window -h`.
    Horizontal,
}

/// The detached resize semantics supported in V1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizePaneAdjustment {
    /// Sets the absolute pane width in columns.
    AbsoluteWidth {
        /// The requested pane width in columns.
        columns: u16,
    },
    /// Sets the absolute pane height in rows.
    AbsoluteHeight {
        /// The requested pane height in rows.
        rows: u16,
    },
    /// Toggles zoom for the targeted pane's window.
    Zoom,
    /// Shrinks the pane height upward by a relative amount.
    Up {
        /// The requested row delta.
        cells: u16,
    },
    /// Grows the pane height downward by a relative amount.
    Down {
        /// The requested row delta.
        cells: u16,
    },
    /// Shrinks the pane width leftward by a relative amount.
    Left {
        /// The requested column delta.
        cells: u16,
    },
    /// Grows the pane width rightward by a relative amount.
    Right {
        /// The requested column delta.
        cells: u16,
    },
    /// Resolves the target and reports success without changing layout.
    NoOp,
    /// Sets the absolute pane width and height.
    AbsoluteSize {
        /// The requested pane width in columns.
        columns: u16,
        /// The requested pane height in rows.
        rows: u16,
    },
    /// Trims lines below the cursor and pulls history into the viewport.
    TrimBelow,
    /// Applies tmux's composed resize form: absolute width/height first, then
    /// one prioritized relative direction.
    Composite {
        /// Optional absolute width in columns.
        columns: Option<u16>,
        /// Optional absolute height in rows.
        rows: Option<u16>,
        /// Optional relative direction applied after absolute dimensions.
        relative: Option<ResizePaneRelativeDirection>,
        /// Relative delta in cells when `relative` is present.
        cells: u16,
    },
}

/// Relative direction used by composed `resize-pane` requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResizePaneRelativeDirection {
    /// Shrinks the pane height upward by a relative amount.
    Up,
    /// Grows the pane height downward by a relative amount.
    Down,
    /// Shrinks the pane width leftward by a relative amount.
    Left,
    /// Grows the pane width rightward by a relative amount.
    Right,
}

impl ResizePaneRelativeDirection {
    /// Converts this relative direction and delta into a regular adjustment.
    #[must_use]
    pub const fn to_adjustment(self, cells: u16) -> ResizePaneAdjustment {
        match self {
            Self::Up => ResizePaneAdjustment::Up { cells },
            Self::Down => ResizePaneAdjustment::Down { cells },
            Self::Left => ResizePaneAdjustment::Left { cells },
            Self::Right => ResizePaneAdjustment::Right { cells },
        }
    }
}
