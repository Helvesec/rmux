use rmux_proto::TerminalSize;

/// Explicit baseline retained once a control constraint affects manual geometry.
/// Before that boundary, ordinary manual windows keep the established no-resize
/// behavior, including their session's outer terminal row basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManualWindowSize {
    pub(super) size: TerminalSize,
    pub(super) constrained: bool,
}

impl ManualWindowSize {
    /// Creates an explicit baseline that has not governed a control resize.
    #[must_use]
    pub const fn new(size: TerminalSize) -> Self {
        Self {
            size,
            constrained: false,
        }
    }

    /// Returns the last explicit, unconstrained dimensions.
    #[must_use]
    pub const fn size(self) -> TerminalSize {
        self.size
    }

    /// Whether a control constraint has activated restoration of this baseline.
    #[must_use]
    pub const fn is_constrained(self) -> bool {
        self.constrained
    }
}

impl super::Window {
    /// Gives a manually sized window its explicit size again after a layout change.
    ///
    /// tmux 3.7b's `recalculate_size()` asks `resize_window()` for
    /// `manual_sx`x`manual_sy` whenever a `window-size manual` window differs
    /// from it, and `resize_window()` keeps the window no smaller than its
    /// layout. A window that a larger layout once held above its explicit
    /// size therefore shrinks back as soon as the layout's minimum falls,
    /// for example when a pane goes away. A constrained baseline is left to the
    /// control-client reconciliation that owns it. Returns whether the window
    /// size changed.
    pub(crate) fn refit_to_manual_size(&mut self) -> bool {
        if self.manual_size.constrained {
            return false;
        }
        let previous = self.size;
        self.set_size(self.manual_size.size);
        self.size != previous
    }
}
