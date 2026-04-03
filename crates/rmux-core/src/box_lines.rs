//! tmux-compatible popup and menu border line styles.

/// Border drawing variants accepted by tmux `*-border-lines` options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxLines {
    /// Single-line Unicode borders.
    #[default]
    Single,
    /// Double-line Unicode borders.
    Double,
    /// Heavy Unicode borders.
    Heavy,
    /// ASCII borders.
    Simple,
    /// Rounded corners with single-line sides.
    Rounded,
    /// Whitespace-only borders.
    Padded,
    /// No border.
    None,
}

impl BoxLines {
    /// Parses tmux option text into a border variant.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("single") {
            "double" => Self::Double,
            "heavy" => Self::Heavy,
            "simple" => Self::Simple,
            "rounded" => Self::Rounded,
            "padded" => Self::Padded,
            "none" => Self::None,
            _ => Self::Single,
        }
    }

    /// Returns whether this border style occupies visible cells.
    #[must_use]
    pub const fn visible(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Returns the vertical border glyph.
    #[must_use]
    pub const fn vertical(self) -> char {
        match self {
            Self::Single | Self::Rounded => '│',
            Self::Double => '║',
            Self::Heavy => '┃',
            Self::Simple => '|',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the horizontal border glyph.
    #[must_use]
    pub const fn horizontal(self) -> char {
        match self {
            Self::Single | Self::Rounded => '─',
            Self::Double => '═',
            Self::Heavy => '━',
            Self::Simple => '-',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the top-left corner glyph.
    #[must_use]
    pub const fn top_left(self) -> char {
        match self {
            Self::Single => '┌',
            Self::Double => '╔',
            Self::Heavy => '┏',
            Self::Simple => '+',
            Self::Rounded => '╭',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the top-right corner glyph.
    #[must_use]
    pub const fn top_right(self) -> char {
        match self {
            Self::Single => '┐',
            Self::Double => '╗',
            Self::Heavy => '┓',
            Self::Simple => '+',
            Self::Rounded => '╮',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the bottom-left corner glyph.
    #[must_use]
    pub const fn bottom_left(self) -> char {
        match self {
            Self::Single => '└',
            Self::Double => '╚',
            Self::Heavy => '┗',
            Self::Simple => '+',
            Self::Rounded => '╰',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the bottom-right corner glyph.
    #[must_use]
    pub const fn bottom_right(self) -> char {
        match self {
            Self::Single => '┘',
            Self::Double => '╝',
            Self::Heavy => '┛',
            Self::Simple => '+',
            Self::Rounded => '╯',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the top separator join glyph.
    #[must_use]
    pub const fn top_join(self) -> char {
        match self {
            Self::Single => '┬',
            Self::Double => '╦',
            Self::Heavy | Self::Rounded => '┳',
            Self::Simple => '+',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the bottom separator join glyph.
    #[must_use]
    pub const fn bottom_join(self) -> char {
        match self {
            Self::Single => '┴',
            Self::Double => '╩',
            Self::Heavy | Self::Rounded => '┻',
            Self::Simple => '+',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the left separator join glyph.
    #[must_use]
    pub const fn left_join(self) -> char {
        match self {
            Self::Single | Self::Rounded => '├',
            Self::Double => '╠',
            Self::Heavy => '┣',
            Self::Simple => '+',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the right separator join glyph.
    #[must_use]
    pub const fn right_join(self) -> char {
        match self {
            Self::Single | Self::Rounded => '┤',
            Self::Double => '╣',
            Self::Heavy => '┫',
            Self::Simple => '+',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the four-way separator join glyph.
    #[must_use]
    pub const fn join(self) -> char {
        match self {
            Self::Single => '┼',
            Self::Double => '╬',
            Self::Heavy | Self::Rounded => '╋',
            Self::Simple => '+',
            Self::Padded | Self::None => ' ',
        }
    }

    /// Returns the tmux outside-cell glyph for this border style.
    #[must_use]
    pub const fn outside(self) -> char {
        match self {
            Self::Single | Self::Double | Self::Heavy | Self::Rounded => '·',
            Self::Simple => '.',
            Self::Padded | Self::None => ' ',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BoxLines;

    #[test]
    fn parses_tmux_popup_border_choices() {
        assert_eq!(BoxLines::parse(None), BoxLines::Single);
        assert_eq!(BoxLines::parse(Some("double")), BoxLines::Double);
        assert_eq!(BoxLines::parse(Some("heavy")), BoxLines::Heavy);
        assert_eq!(BoxLines::parse(Some("simple")), BoxLines::Simple);
        assert_eq!(BoxLines::parse(Some("rounded")), BoxLines::Rounded);
        assert_eq!(BoxLines::parse(Some("padded")), BoxLines::Padded);
        assert_eq!(BoxLines::parse(Some("none")), BoxLines::None);
        assert_eq!(BoxLines::parse(Some("unknown")), BoxLines::Single);
    }

    #[test]
    fn exposes_tmux_join_glyphs_for_all_border_sets() {
        assert_eq!(BoxLines::Single.top_join(), '┬');
        assert_eq!(BoxLines::Single.bottom_join(), '┴');
        assert_eq!(BoxLines::Single.join(), '┼');

        assert_eq!(BoxLines::Double.top_join(), '╦');
        assert_eq!(BoxLines::Double.bottom_join(), '╩');
        assert_eq!(BoxLines::Double.join(), '╬');

        assert_eq!(BoxLines::Heavy.top_join(), '┳');
        assert_eq!(BoxLines::Heavy.bottom_join(), '┻');
        assert_eq!(BoxLines::Heavy.join(), '╋');

        assert_eq!(BoxLines::Simple.top_join(), '+');
        assert_eq!(BoxLines::Simple.bottom_join(), '+');
        assert_eq!(BoxLines::Simple.join(), '+');

        assert_eq!(BoxLines::Rounded.top_join(), '┳');
        assert_eq!(BoxLines::Rounded.bottom_join(), '┻');
        assert_eq!(BoxLines::Rounded.join(), '╋');

        assert_eq!(BoxLines::Padded.top_join(), ' ');
        assert_eq!(BoxLines::Padded.bottom_join(), ' ');
        assert_eq!(BoxLines::Padded.join(), ' ');

        assert_eq!(BoxLines::None.top_join(), ' ');
        assert_eq!(BoxLines::None.bottom_join(), ' ');
        assert_eq!(BoxLines::None.join(), ' ');
    }

    #[test]
    fn outside_glyph_matches_tmux_border_tables() {
        assert_eq!(BoxLines::Single.outside(), '·');
        assert_eq!(BoxLines::Double.outside(), '·');
        assert_eq!(BoxLines::Heavy.outside(), '·');
        assert_eq!(BoxLines::Simple.outside(), '.');
        assert_eq!(BoxLines::Rounded.outside(), '·');
        assert_eq!(BoxLines::Padded.outside(), ' ');
        assert_eq!(BoxLines::None.outside(), ' ');
    }
}
