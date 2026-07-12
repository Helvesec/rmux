use rmux_proto::{HookLifecycle, HookName, PaneId, SessionName, WindowId};

/// The global root used by a hook inventory query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookGlobalRoot {
    /// Session-scoped hooks stored at the global session root.
    Session,
    /// Window- and pane-scoped hooks stored at the global window root.
    Window,
}

/// Indexed mutation options for `set-hook`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookSetOptions {
    /// Whether the new command should be appended to the next free array slot.
    pub append: bool,
    /// The explicit array index to replace, when present.
    pub index: Option<u32>,
}

/// Stable identity used to address a hook scope after target resolution.
///
/// Window and pane bindings use server-wide identities so linked windows and
/// grouped-session aliases share one logical binding. The session name stays
/// attached to window and pane scopes because session hooks remain local to a
/// session even when its windows are shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookScopeIdentity {
    /// A global hook root.
    Global,
    /// A session-local hook scope.
    Session(SessionName),
    /// A window-local hook scope.
    Window {
        /// Session through which the window was addressed.
        session_name: SessionName,
        /// Stable identity shared by every link to the window.
        window_id: WindowId,
    },
    /// A pane-local hook scope.
    Pane {
        /// Session through which the pane was addressed.
        session_name: SessionName,
        /// Stable identity of the pane's containing window.
        window_id: WindowId,
        /// Stable identity shared by every alias of the pane.
        pane_id: PaneId,
    },
}

impl HookScopeIdentity {
    pub(super) const fn session_name(&self) -> Option<&SessionName> {
        match self {
            Self::Session(session_name)
            | Self::Window { session_name, .. }
            | Self::Pane { session_name, .. } => Some(session_name),
            Self::Global => None,
        }
    }
}

/// A rendered hook binding snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookBindingView {
    pub(super) hook: HookName,
    pub(super) index: u32,
    pub(super) command: String,
    pub(super) lifecycle: HookLifecycle,
}

impl HookBindingView {
    /// Returns the bound hook name.
    #[must_use]
    pub const fn hook(&self) -> HookName {
        self.hook
    }

    /// Returns the bound array index.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Returns the stored command string.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Returns the stored lifecycle.
    #[must_use]
    pub const fn lifecycle(&self) -> HookLifecycle {
        self.lifecycle
    }
}

/// The command payload emitted when a hook dispatches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDispatch {
    pub(super) command: String,
    pub(super) lifecycle: HookLifecycle,
}

impl HookDispatch {
    /// Returns the exact shell command that should be executed.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Returns the lifecycle of the dispatched hook.
    #[must_use]
    pub const fn lifecycle(&self) -> HookLifecycle {
        self.lifecycle
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HookClass {
    Session,
    Window,
    Pane,
}
