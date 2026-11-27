//! `GridCell` construction, accessors and style predicates, and the inline
//! storage of its text.

use compact_str::CompactString;

use crate::input::{CellState, Colour, COLOUR_DEFAULT};
use crate::utf8::CombinedSequenceDiscard;

use super::{CellText, GridCell, GridCellFlags};

impl CellText {
    fn from_char(ch: char) -> Self {
        let mut inline = [0_u8; 4];
        let inline_len = ch.encode_utf8(&mut inline).len() as u8;
        Self {
            inline,
            inline_len,
            extended: None,
        }
    }

    fn from_ascii_byte(byte: u8) -> Self {
        debug_assert!(byte.is_ascii());
        Self {
            inline: [byte, 0, 0, 0],
            inline_len: 1,
            extended: None,
        }
    }

    fn new(text: String) -> Self {
        let mut chars = text.chars();
        if let Some(ch) = chars.next() {
            if chars.next().is_none() {
                return Self::from_char(ch);
            }
        }
        Self {
            inline: [0_u8; 4],
            inline_len: 0,
            extended: Some(Box::new(CompactString::new(text))),
        }
    }

    pub(in crate::grid) fn as_str(&self) -> &str {
        if let Some(text) = &self.extended {
            return text.as_str();
        }
        std::str::from_utf8(&self.inline[..usize::from(self.inline_len)])
            .expect("inline cell text must be valid utf-8")
    }

    fn is_single_space(&self) -> bool {
        self.extended.is_none() && self.inline_len == 1 && self.inline[0] == b' '
    }
}

impl Default for GridCell {
    fn default() -> Self {
        Self::blank_with_bg(COLOUR_DEFAULT)
    }
}

impl GridCell {
    /// Creates a blank cell with the given background colour.
    #[must_use]
    pub fn blank_with_bg(bg: Colour) -> Self {
        Self {
            text: CellText::from_char(' '),
            width: 1,
            flags: GridCellFlags::CLEARED,
            combined_sequence_discard: CombinedSequenceDiscard::default(),
            attr: 0,
            fg: COLOUR_DEFAULT,
            bg,
            us: COLOUR_DEFAULT,
            link: 0,
        }
    }

    /// Creates a printable cell from the parser cell state.
    #[must_use]
    pub fn from_state(ch: char, width: u8, state: &CellState, flags: GridCellFlags) -> Self {
        let mut resolved_flags = flags;
        resolved_flags.remove(GridCellFlags::CLEARED);
        Self {
            text: CellText::from_char(ch),
            width,
            flags: resolved_flags,
            combined_sequence_discard: CombinedSequenceDiscard::default(),
            attr: state.attr(),
            fg: state.fg(),
            bg: state.bg(),
            us: state.us(),
            link: state.link(),
        }
    }

    pub(in crate::grid) fn from_plain_ascii(byte: u8) -> Self {
        Self {
            text: CellText::from_ascii_byte(byte),
            width: 1,
            flags: GridCellFlags::default(),
            combined_sequence_discard: CombinedSequenceDiscard::default(),
            attr: 0,
            fg: COLOUR_DEFAULT,
            bg: COLOUR_DEFAULT,
            us: COLOUR_DEFAULT,
            link: 0,
        }
    }

    pub(super) fn set_plain_ascii(&mut self, byte: u8) {
        self.text = CellText::from_ascii_byte(byte);
        self.width = 1;
        self.flags = GridCellFlags::default();
        self.combined_sequence_discard = CombinedSequenceDiscard::default();
        self.attr = 0;
        self.fg = COLOUR_DEFAULT;
        self.bg = COLOUR_DEFAULT;
        self.us = COLOUR_DEFAULT;
        self.link = 0;
    }

    /// Returns the stored text payload.
    #[must_use]
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    /// Returns the display width of the cell.
    #[must_use]
    pub const fn width(&self) -> u8 {
        self.width
    }

    /// Returns the cell flags.
    #[must_use]
    pub const fn flags(&self) -> GridCellFlags {
        self.flags
    }

    /// Returns whether this cell is a padding cell.
    #[must_use]
    pub const fn is_padding(&self) -> bool {
        self.flags.contains(GridCellFlags::PADDING)
    }

    /// Returns whether this blank cell exists only to place a following wide glyph.
    #[must_use]
    pub const fn is_reflow_gap(&self) -> bool {
        self.flags.contains(GridCellFlags::REFLOW_GAP)
    }

    /// Returns whether the cell is visually blank.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.flags.contains(GridCellFlags::CLEARED)
            && !self.flags.contains(GridCellFlags::PADDING)
            && self.width == 1
            && self.text.is_single_space()
            && self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.bg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
    }

    /// Whether this cell holds no written content, whatever background it was
    /// painted with.
    ///
    /// This is [`GridCell::is_blank`] without its background condition. An
    /// erase paints a background over cells nobody wrote to, and tmux likewise
    /// keeps `cellused` at zero when `grid_empty_line` re-materialises a line
    /// with a background. A space someone actually typed, a style-bearing cell
    /// and the padding of a wide glyph all remain content, because none of them
    /// carries the `CLEARED` flag. The compact ASCII branch of
    /// [`GridLine::used_end`] answers from its cached text instead and keeps
    /// its own established trailing-space behaviour; this predicate governs
    /// the materialised-cell branch.
    ///
    /// [`GridLine::used_end`]: super::GridLine::used_end
    pub(super) fn is_logically_empty(&self) -> bool {
        self.flags.contains(GridCellFlags::CLEARED)
            && !self.flags.contains(GridCellFlags::PADDING)
            && self.width == 1
            && self.text.is_single_space()
            && self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
    }

    pub(super) fn is_explicit_default_space(&self) -> bool {
        !self.flags.contains(GridCellFlags::CLEARED)
            && !self.flags.contains(GridCellFlags::PADDING)
            && self.width == 1
            && self.text.is_single_space()
            && self.has_default_style()
    }

    fn has_default_style(&self) -> bool {
        self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.bg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
    }

    pub(super) fn has_non_default_style(&self) -> bool {
        !self.has_default_style()
    }

    pub(crate) fn set_text(&mut self, text: String) {
        self.text = CellText::new(text);
    }

    pub(crate) fn set_width(&mut self, width: u8) {
        self.width = width;
    }

    pub(crate) fn set_flags(&mut self, flags: GridCellFlags) {
        self.flags = flags;
    }

    pub(crate) const fn combined_sequence_discard(&self) -> CombinedSequenceDiscard {
        self.combined_sequence_discard
    }

    pub(crate) fn combined_sequence_discard_mut(&mut self) -> &mut CombinedSequenceDiscard {
        &mut self.combined_sequence_discard
    }

    pub(crate) fn clear_combined_sequence_discard(&mut self) {
        self.combined_sequence_discard.clear();
    }

    pub(crate) fn set_attr(&mut self, attr: u16) {
        self.attr = attr;
    }

    pub(crate) fn set_fg(&mut self, fg: Colour) {
        self.fg = fg;
    }

    pub(crate) fn set_bg(&mut self, bg: Colour) {
        self.bg = bg;
    }

    pub(crate) fn set_us(&mut self, us: Colour) {
        self.us = us;
    }

    pub(super) fn is_plain_default_ascii(&self) -> bool {
        self.width == 1
            && self.flags == GridCellFlags::default()
            && self.attr == 0
            && self.fg == COLOUR_DEFAULT
            && self.bg == COLOUR_DEFAULT
            && self.us == COLOUR_DEFAULT
            && self.link == 0
            && self.text.as_str().is_ascii()
            && self.text.as_str().len() == 1
    }
}
