//! Vocabulary of the v1 frame ledger: kinds, directions, statuses, features and entries.

use super::DIRECTION_BIT;

/// Stable v1 frame identifier for a specific DTO type and direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameKind(pub u16);

impl FrameKind {
    /// Direction encoded in the high bit of the kind value.
    #[must_use]
    pub const fn direction(self) -> FrameDirection {
        if self.0 & DIRECTION_BIT == 0 {
            FrameDirection::ClientToServer
        } else {
            FrameDirection::ServerToClient
        }
    }

    /// Returns the bincode tag value (low 15 bits) pinned by this ledger entry.
    #[must_use]
    pub const fn bincode_tag(self) -> u32 {
        (self.0 & !DIRECTION_BIT) as u32
    }
}

/// Direction of a v1 ledger frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FrameDirection {
    /// Frames sent from client to server.
    ClientToServer,
    /// Frames sent from server to client.
    ServerToClient,
}

impl FrameDirection {
    /// Returns the human-readable wire direction abbreviation.
    #[must_use]
    pub const fn abbreviation(self) -> &'static str {
        match self {
            Self::ClientToServer => "c2s",
            Self::ServerToClient => "s2c",
        }
    }
}

/// Lifecycle status for a ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameStatus {
    /// The entry is in use by the current build.
    Active,
    /// The entry is permanently reserved and must not be reused.
    Reserved,
}

/// Owning feature area for the ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameFeature {
    /// Session lifecycle (`new-session`, `kill-session`, ...).
    Sessions,
    /// Window lifecycle and layout (`new-window`, `kill-window`, ...).
    Windows,
    /// Pane lifecycle, input, and listing (`split-window`, `send-keys`, ...).
    Panes,
    /// Layout selection (`select-layout`, `next-layout`, ...).
    Layouts,
    /// Buffer storage (`set-buffer`, `paste-buffer`, ...).
    Buffers,
    /// Format expansion (`display-message`, `if-shell`, ...).
    Formats,
    /// Hook lifecycle (`set-hook`, `show-hooks`, ...).
    Hooks,
    /// Server option configuration (`set-option`, `show-options`, ...).
    Options,
    /// Server lifecycle (`kill-server`, `lock-server`, ...).
    Server,
    /// Client lifecycle (`attach-session`, `refresh-client`, ...).
    Clients,
    /// Key bindings (`bind-key`, `list-keys`, ...).
    Keys,
    /// Internal target resolution.
    Targets,
    /// Control-mode upgrade and clock-mode mounts.
    Control,
    /// Version and capability negotiation.
    Protocol,
    /// Daemon-backed SDK waits and cancellation.
    SdkWaits,
    /// Error envelopes.
    Errors,
    /// Browser-visible pane sharing.
    Web,
    /// Reserved for future extensions.
    Reserved,
}

/// Single v1 wire ledger entry.
#[derive(Debug, Clone, Copy)]
pub struct FrameLedgerEntry {
    /// Stable `u16` ledger identifier.
    pub kind: FrameKind,
    /// Direction of the frame.
    pub direction: FrameDirection,
    /// Lifecycle status.
    pub status: FrameStatus,
    /// Rust DTO type name.
    pub dto_type: &'static str,
    /// Owning feature area.
    pub feature: FrameFeature,
    /// Optional fixture file name (no extension) under
    /// `tests/wire-fixtures/ledger-v1-current-wire/`.
    pub fixture: Option<&'static str>,
    /// Compatibility note for the entry.
    pub compatibility_note: &'static str,
}
