use rustix::termios::Winsize;

/// Terminal geometry in columns and rows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct TerminalSize {
    /// The terminal width in character cells.
    pub cols: u16,
    /// The terminal height in character cells.
    pub rows: u16,
}

impl TerminalSize {
    /// Creates a new terminal size value.
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    pub(crate) const fn into_winsize(self) -> Winsize {
        Winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    pub(crate) const fn from_winsize(winsize: Winsize) -> Self {
        Self {
            cols: winsize.ws_col,
            rows: winsize.ws_row,
        }
    }
}
