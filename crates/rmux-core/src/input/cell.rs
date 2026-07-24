//! Cell state (attributes, colours, character set).

use super::colour::{Colour, COLOUR_DEFAULT};

/// Grid cell attributes matching tmux `GRID_ATTR_*`.
#[allow(non_snake_case, non_upper_case_globals)]
pub mod GridAttr {
    /// Bold.
    pub const BRIGHT: u16 = 0x1;
    /// Dim.
    pub const DIM: u16 = 0x2;
    /// Single underline.
    pub const UNDERSCORE: u16 = 0x4;
    /// Blink.
    pub const BLINK: u16 = 0x8;
    /// Reverse video.
    pub const REVERSE: u16 = 0x10;
    /// Hidden.
    pub const HIDDEN: u16 = 0x20;
    /// Italics.
    pub const ITALICS: u16 = 0x40;
    /// ACS line-drawing charset.
    pub const CHARSET: u16 = 0x80;
    /// Strikethrough.
    pub const STRIKETHROUGH: u16 = 0x100;
    /// Double underline.
    pub const UNDERSCORE_2: u16 = 0x200;
    /// Curly underline.
    pub const UNDERSCORE_3: u16 = 0x400;
    /// Dotted underline.
    pub const UNDERSCORE_4: u16 = 0x800;
    /// Dashed underline.
    pub const UNDERSCORE_5: u16 = 0x1000;
    /// Overline.
    pub const OVERLINE: u16 = 0x2000;
    /// Explicitly no inherited attributes.
    pub const NOATTR: u16 = 0x4000;

    /// All underscore variants combined.
    pub const ALL_UNDERSCORE: u16 =
        UNDERSCORE | UNDERSCORE_2 | UNDERSCORE_3 | UNDERSCORE_4 | UNDERSCORE_5;
}

/// Grid cell, holding fg/bg/us colours and attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GridCell {
    pub attr: u16,
    pub fg: Colour,
    pub bg: Colour,
    pub us: Colour,
    pub link: u32,
}

impl Default for GridCell {
    fn default() -> Self {
        Self {
            attr: 0,
            fg: COLOUR_DEFAULT,
            bg: COLOUR_DEFAULT,
            us: COLOUR_DEFAULT,
            link: 0,
        }
    }
}

/// Character set and cell state for the parser, matching tmux `input_cell`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellState {
    /// Grid cell attributes.
    pub(crate) cell: GridCell,
    /// Active character set: 0 = G0, 1 = G1.
    pub set: i32,
    /// G0 set: 0 = normal, 1 = ACS.
    pub g0set: i32,
    /// G1 set: 0 = normal, 1 = ACS.
    pub g1set: i32,
}

impl CellState {
    /// Returns the current foreground colour.
    #[must_use]
    pub fn fg(&self) -> Colour {
        self.cell.fg
    }

    /// Returns the current background colour.
    #[must_use]
    pub fn bg(&self) -> Colour {
        self.cell.bg
    }

    /// Returns the current underline colour.
    #[must_use]
    pub fn us(&self) -> Colour {
        self.cell.us
    }

    /// Returns the current attribute flags.
    #[must_use]
    pub fn attr(&self) -> u16 {
        self.cell.attr
    }

    /// Returns the hyperlink ID.
    #[must_use]
    pub fn link(&self) -> u32 {
        self.cell.link
    }

    pub(crate) fn reset(&mut self) {
        self.cell = GridCell::default();
        self.set = 0;
        self.g0set = 0;
        self.g1set = 0;
    }
}

/// Saved state for DECSC/DECRC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedState {
    pub(crate) cell: CellState,
    pub(crate) cx: u32,
    pub(crate) cy: u32,
    pub(crate) mode_origin: bool,
}

impl SavedState {
    /// Returns the saved rendition and character-set state.
    #[must_use]
    pub const fn cell(&self) -> &CellState {
        &self.cell
    }

    /// Returns the cursor saved by DECSC/SCP.
    #[must_use]
    pub const fn cursor_position(&self) -> (u32, u32) {
        (self.cx, self.cy)
    }

    /// Returns whether origin mode was active at save time.
    #[must_use]
    pub const fn origin_mode(&self) -> bool {
        self.mode_origin
    }
}
