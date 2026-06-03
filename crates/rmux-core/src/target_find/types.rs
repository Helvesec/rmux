use std::collections::HashMap;

use rmux_proto::{SessionName, Target};

/// The target type requested by a tmux command's `cmd_entry_flag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetFindType {
    /// Resolve to a session target.
    Session,
    /// Resolve to a window target.
    Window,
    /// Resolve to a pane target.
    Pane,
}

/// Flag bitset matching tmux `CMD_FIND_*` resolution controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetFindFlags(u8);

impl TargetFindFlags {
    /// No find flags.
    pub const NONE: Self = Self(0);
    /// Prefer unattached sessions when selecting a best session.
    pub const PREFER_UNATTACHED: Self = Self(1 << 0);
    /// Suppress emitted target lookup diagnostics.
    pub const QUIET: Self = Self(1 << 1);
    /// Allow a window target to resolve to a nonexistent index.
    pub const WINDOW_INDEX: Self = Self(1 << 2);
    /// Use the marked pane as the default current target.
    pub const DEFAULT_MARKED: Self = Self(1 << 3);
    /// Suppress session prefix and fnmatch fallback.
    pub const EXACT_SESSION: Self = Self(1 << 4);
    /// Suppress window prefix and fnmatch fallback.
    pub const EXACT_WINDOW: Self = Self(1 << 5);
    /// Allow target lookup failure to be represented by the caller.
    pub const CANFAIL: Self = Self(1 << 6);

    /// Returns a bitset containing both flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns whether every bit in `other` is present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub(super) fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// Raw command target text before tmux-style lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedTarget {
    value: Option<String>,
}

impl UnresolvedTarget {
    /// Builds an unresolved target from a command argument string.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: Some(value.into()),
        }
    }

    /// Builds an omitted target, which resolves through the current target.
    #[must_use]
    pub const fn none() -> Self {
        Self { value: None }
    }

    /// Returns the raw argument text, when a target was supplied.
    #[must_use]
    pub fn as_deref(&self) -> Option<&str> {
        self.value.as_deref()
    }
}

/// Current target context used for omitted targets and relative lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetFindContext {
    current: Option<Target>,
    mouse_target: Option<Target>,
    marked_target: Option<Target>,
    pane_base_indices: HashMap<(SessionName, u32), u32>,
}

impl TargetFindContext {
    /// Creates a context with an optional current target.
    #[must_use]
    pub fn new(current: Option<Target>) -> Self {
        Self {
            current,
            mouse_target: None,
            marked_target: None,
            pane_base_indices: HashMap::new(),
        }
    }

    /// Creates a context anchored to a concrete current target.
    #[must_use]
    pub fn from_target(target: Target) -> Self {
        Self {
            current: Some(target),
            mouse_target: None,
            marked_target: None,
            pane_base_indices: HashMap::new(),
        }
    }

    /// Returns the current target when one is available.
    #[must_use]
    pub const fn current(&self) -> Option<&Target> {
        self.current.as_ref()
    }

    /// Returns a context extended with an active mouse target.
    #[must_use]
    pub fn with_mouse_target(mut self, mouse_target: Option<Target>) -> Self {
        self.mouse_target = mouse_target;
        self
    }

    /// Returns the active mouse target when one is available.
    #[must_use]
    pub const fn mouse_target(&self) -> Option<&Target> {
        self.mouse_target.as_ref()
    }

    /// Returns a context extended with the server-wide marked pane target.
    #[must_use]
    pub fn with_marked_target(mut self, marked_target: Option<Target>) -> Self {
        self.marked_target = marked_target;
        self
    }

    /// Returns the marked pane target when one is available.
    #[must_use]
    pub const fn marked_target(&self) -> Option<&Target> {
        self.marked_target.as_ref()
    }

    /// Returns a context extended with visible pane-index bases.
    #[must_use]
    pub fn with_pane_base_indices(
        mut self,
        pane_base_indices: HashMap<(SessionName, u32), u32>,
    ) -> Self {
        self.pane_base_indices = pane_base_indices;
        self
    }

    pub(super) fn pane_base_index(&self, session_name: &SessionName, window_index: u32) -> u32 {
        self.pane_base_indices
            .get(&(session_name.clone(), window_index))
            .copied()
            .unwrap_or(0)
    }
}

/// Per-command source or target lookup metadata from frozen tmux `cmd_entry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandTargetSpec {
    /// The command flag that carries the target text.
    pub flag: char,
    /// The requested find result type.
    pub find_type: TargetFindType,
    /// The tmux `CMD_FIND_*` flags for this target.
    pub flags: TargetFindFlags,
}

/// Per-command source and target lookup metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandTargetMetadata {
    /// Metadata for the source target flag, when the command has one.
    pub source: Option<CommandTargetSpec>,
    /// Metadata for the target flag, when the command has one.
    pub target: Option<CommandTargetSpec>,
}
