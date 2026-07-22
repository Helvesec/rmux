use crate::{PaneOutputStream, PaneSnapshot};

/// Complete daemon-rendered terminal state and its exact post-keyframe byte stream.
pub struct PaneOutputRecovery {
    /// Terminal width represented by [`Self::keyframe`].
    pub cols: u16,
    /// Terminal height represented by [`Self::keyframe`].
    pub rows: u16,
    /// ANSI bytes that reset and reconstruct the renderer state.
    pub keyframe: Vec<u8>,
    /// Typed pane grid captured at the same boundary as [`Self::keyframe`].
    pub snapshot: PaneSnapshot,
    /// Whether the captured pane is using the alternate screen.
    pub alternate: bool,
    /// Exact first output sequence not represented by the keyframe.
    pub next_sequence: u64,
    /// Raw post-keyframe output, preserving bytes and bounded lag notices.
    pub output: PaneOutputStream,
}
