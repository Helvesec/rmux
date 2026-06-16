use rmux_proto::{HookName, PaneTarget, ScopeSelector, SessionName, Target, WindowTarget};

/// A typed server lifecycle event that may dispatch a registered hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// A client attached to a session.
    ClientAttached {
        /// The session associated with the event.
        session_name: SessionName,
        /// The best available rmux client identifier.
        client_name: Option<String>,
    },
    /// A client detached from a session.
    ClientDetached {
        /// The session associated with the event.
        session_name: SessionName,
        /// The best available rmux client identifier.
        client_name: Option<String>,
    },
    /// A client switched to another session.
    ClientSessionChanged {
        /// The session associated with the event.
        session_name: SessionName,
        /// The best available rmux client identifier.
        client_name: Option<String>,
    },
    /// A session was created.
    SessionCreated {
        /// The session associated with the event.
        session_name: SessionName,
    },
    /// A session was closed.
    SessionClosed {
        /// The session associated with the event.
        session_name: SessionName,
        /// The removed session ID captured at notification time, when known.
        session_id: Option<u32>,
    },
    /// A session was renamed.
    SessionRenamed {
        /// The session associated with the event.
        session_name: SessionName,
    },
    /// A session's active window changed.
    SessionWindowChanged {
        /// The session associated with the event.
        session_name: SessionName,
    },
    /// A window was linked into a session.
    WindowLinked {
        /// The session associated with the event.
        session_name: SessionName,
        /// The linked window, when it is known at the call site.
        target: Option<WindowTarget>,
    },
    /// A window was unlinked from a session.
    WindowUnlinked {
        /// The session associated with the event.
        session_name: SessionName,
        /// The unlinked window, when it is known at the call site.
        target: Option<WindowTarget>,
        /// The removed window ID captured at notification time, when known.
        window_id: Option<u32>,
        /// The removed window name captured at notification time, when known.
        window_name: Option<String>,
    },
    /// A window was renamed.
    WindowRenamed {
        /// The targeted window.
        target: WindowTarget,
    },
    /// A window layout changed.
    WindowLayoutChanged {
        /// The targeted window.
        target: WindowTarget,
    },
    /// A window size changed.
    WindowResized {
        /// The targeted window.
        target: WindowTarget,
    },
    /// A window's active pane changed.
    WindowPaneChanged {
        /// The targeted window.
        target: WindowTarget,
    },
    /// A bell alert fired for a window.
    AlertBell {
        /// The targeted window.
        target: WindowTarget,
    },
    /// An activity alert fired for a window.
    AlertActivity {
        /// The targeted window.
        target: WindowTarget,
    },
    /// A silence alert fired for a window.
    AlertSilence {
        /// The targeted window.
        target: WindowTarget,
    },
    /// A pane exited or was removed from service.
    PaneExited {
        /// The targeted pane.
        target: PaneTarget,
        /// The removed pane ID captured at notification time, when known.
        pane_id: Option<u32>,
        /// The removed window ID captured at notification time, when known.
        window_id: Option<u32>,
        /// The removed window name captured at notification time, when known.
        window_name: Option<String>,
    },
    /// A pane died and was kept by `remain-on-exit`.
    PaneDied {
        /// The targeted pane.
        target: PaneTarget,
        /// The dead pane ID captured at notification time, when known.
        pane_id: Option<u32>,
        /// The dead pane's window ID captured at notification time, when known.
        window_id: Option<u32>,
        /// The dead pane's window name captured at notification time, when known.
        window_name: Option<String>,
    },
    /// A pane mode or display overlay state changed.
    PaneModeChanged {
        /// The targeted pane.
        target: PaneTarget,
    },
    /// A paste buffer was created or replaced.
    PasteBufferChanged {
        /// The affected paste buffer name.
        buffer_name: String,
    },
    /// A paste buffer was deleted or evicted.
    PasteBufferDeleted {
        /// The affected paste buffer name.
        buffer_name: String,
    },
    /// A `select-window`-family command completed successfully.
    AfterSelectWindow {
        /// The selected window.
        target: WindowTarget,
    },
    /// A `select-pane`-family command completed successfully.
    AfterSelectPane {
        /// The selected pane.
        target: PaneTarget,
    },
    /// A `send-keys` command completed successfully.
    AfterSendKeys {
        /// The targeted pane.
        target: PaneTarget,
    },
    /// A `set-option` command completed successfully.
    AfterSetOption {
        /// The session associated with the option scope, when one exists.
        session_name: Option<SessionName>,
    },
}

impl LifecycleEvent {
    /// Returns the hook name corresponding to this lifecycle event.
    #[must_use]
    pub const fn hook_name(&self) -> HookName {
        match self {
            Self::ClientAttached { .. } => HookName::ClientAttached,
            Self::ClientDetached { .. } => HookName::ClientDetached,
            Self::ClientSessionChanged { .. } => HookName::ClientSessionChanged,
            Self::SessionCreated { .. } => HookName::SessionCreated,
            Self::SessionClosed { .. } => HookName::SessionClosed,
            Self::SessionRenamed { .. } => HookName::SessionRenamed,
            Self::SessionWindowChanged { .. } => HookName::SessionWindowChanged,
            Self::WindowLinked { .. } => HookName::WindowLinked,
            Self::WindowUnlinked { .. } => HookName::WindowUnlinked,
            Self::WindowRenamed { .. } => HookName::WindowRenamed,
            Self::WindowLayoutChanged { .. } => HookName::WindowLayoutChanged,
            Self::WindowResized { .. } => HookName::WindowResized,
            Self::WindowPaneChanged { .. } => HookName::WindowPaneChanged,
            Self::AlertBell { .. } => HookName::AlertBell,
            Self::AlertActivity { .. } => HookName::AlertActivity,
            Self::AlertSilence { .. } => HookName::AlertSilence,
            Self::PaneExited { .. } => HookName::PaneExited,
            Self::PaneDied { .. } => HookName::PaneDied,
            Self::PaneModeChanged { .. } => HookName::PaneModeChanged,
            Self::PasteBufferChanged { .. } => HookName::PasteBufferChanged,
            Self::PasteBufferDeleted { .. } => HookName::PasteBufferDeleted,
            Self::AfterSelectWindow { .. } => HookName::AfterSelectWindow,
            Self::AfterSelectPane { .. } => HookName::AfterSelectPane,
            Self::AfterSendKeys { .. } => HookName::AfterSendKeys,
            Self::AfterSetOption { .. } => HookName::AfterSetOption,
        }
    }

    /// Returns the event scope used for hook resolution.
    #[must_use]
    pub fn scope(&self) -> ScopeSelector {
        match self {
            Self::ClientAttached { session_name, .. }
            | Self::ClientDetached { session_name, .. }
            | Self::ClientSessionChanged { session_name, .. }
            | Self::SessionCreated { session_name }
            | Self::SessionClosed { session_name, .. }
            | Self::SessionRenamed { session_name }
            | Self::SessionWindowChanged { session_name }
            | Self::WindowLinked { session_name, .. }
            | Self::WindowUnlinked { session_name, .. } => {
                ScopeSelector::Session(session_name.clone())
            }
            Self::WindowRenamed { target }
            | Self::WindowLayoutChanged { target }
            | Self::WindowResized { target }
            | Self::WindowPaneChanged { target }
            | Self::AlertBell { target }
            | Self::AlertActivity { target }
            | Self::AlertSilence { target }
            | Self::AfterSelectWindow { target } => ScopeSelector::Window(target.clone()),
            Self::PaneExited { target, .. }
            | Self::PaneDied { target, .. }
            | Self::PaneModeChanged { target }
            | Self::AfterSelectPane { target }
            | Self::AfterSendKeys { target } => ScopeSelector::Pane(target.clone()),
            Self::PasteBufferChanged { .. } | Self::PasteBufferDeleted { .. } => {
                ScopeSelector::Global
            }
            Self::AfterSetOption {
                session_name: Some(session_name),
            } => ScopeSelector::Session(session_name.clone()),
            Self::AfterSetOption { session_name: None } => ScopeSelector::Global,
        }
    }

    /// Returns the session associated with this lifecycle event, when one exists.
    #[must_use]
    pub fn session_name(&self) -> Option<&SessionName> {
        match self {
            Self::ClientAttached { session_name, .. }
            | Self::ClientDetached { session_name, .. }
            | Self::ClientSessionChanged { session_name, .. }
            | Self::SessionCreated { session_name }
            | Self::SessionClosed { session_name, .. }
            | Self::SessionRenamed { session_name }
            | Self::SessionWindowChanged { session_name }
            | Self::WindowLinked { session_name, .. }
            | Self::WindowUnlinked { session_name, .. } => Some(session_name),
            Self::WindowRenamed { target }
            | Self::WindowLayoutChanged { target }
            | Self::WindowResized { target }
            | Self::WindowPaneChanged { target }
            | Self::AlertBell { target }
            | Self::AlertActivity { target }
            | Self::AlertSilence { target }
            | Self::AfterSelectWindow { target } => Some(target.session_name()),
            Self::PaneExited { target, .. }
            | Self::PaneDied { target, .. }
            | Self::PaneModeChanged { target }
            | Self::AfterSelectPane { target }
            | Self::AfterSendKeys { target } => Some(target.session_name()),
            Self::AfterSetOption { session_name } => session_name.as_ref(),
            Self::PasteBufferChanged { .. } | Self::PasteBufferDeleted { .. } => None,
        }
    }

    /// Returns the event client identifier when one is available.
    #[must_use]
    pub fn client_name(&self) -> Option<&str> {
        match self {
            Self::ClientAttached { client_name, .. }
            | Self::ClientDetached { client_name, .. }
            | Self::ClientSessionChanged { client_name, .. } => client_name.as_deref(),
            _ => None,
        }
    }

    /// Returns the event window target when one is available.
    #[must_use]
    pub fn window_target(&self) -> Option<WindowTarget> {
        match self {
            Self::WindowLinked { target, .. } | Self::WindowUnlinked { target, .. } => {
                target.clone()
            }
            Self::WindowRenamed { target }
            | Self::WindowLayoutChanged { target }
            | Self::WindowResized { target }
            | Self::WindowPaneChanged { target }
            | Self::AlertBell { target }
            | Self::AlertActivity { target }
            | Self::AlertSilence { target }
            | Self::AfterSelectWindow { target } => Some(target.clone()),
            Self::PaneExited { target, .. }
            | Self::PaneDied { target, .. }
            | Self::PaneModeChanged { target }
            | Self::AfterSelectPane { target }
            | Self::AfterSendKeys { target } => Some(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )),
            _ => None,
        }
    }

    /// Returns the event pane target when one is available.
    #[must_use]
    pub fn pane_target(&self) -> Option<&PaneTarget> {
        match self {
            Self::PaneExited { target, .. }
            | Self::PaneDied { target, .. }
            | Self::PaneModeChanged { target }
            | Self::AfterSelectPane { target }
            | Self::AfterSendKeys { target } => Some(target),
            _ => None,
        }
    }

    /// Returns the event session ID captured at notification time, when one exists.
    #[must_use]
    pub const fn session_id(&self) -> Option<u32> {
        match self {
            Self::SessionClosed { session_id, .. } => *session_id,
            _ => None,
        }
    }

    /// Returns the event window ID captured at notification time, when one exists.
    #[must_use]
    pub const fn window_id(&self) -> Option<u32> {
        match self {
            Self::WindowUnlinked { window_id, .. }
            | Self::PaneExited { window_id, .. }
            | Self::PaneDied { window_id, .. } => *window_id,
            _ => None,
        }
    }

    /// Returns the event window name captured at notification time, when one exists.
    #[must_use]
    pub fn window_name_snapshot(&self) -> Option<&str> {
        match self {
            Self::WindowUnlinked { window_name, .. }
            | Self::PaneExited { window_name, .. }
            | Self::PaneDied { window_name, .. } => window_name.as_deref(),
            _ => None,
        }
    }

    /// Returns the event pane ID captured at notification time, when one exists.
    #[must_use]
    pub const fn pane_id(&self) -> Option<u32> {
        match self {
            Self::PaneExited { pane_id, .. } | Self::PaneDied { pane_id, .. } => *pane_id,
            _ => None,
        }
    }

    /// Returns the paste buffer name when one is available.
    #[must_use]
    pub fn buffer_name(&self) -> Option<&str> {
        match self {
            Self::PasteBufferChanged { buffer_name } | Self::PasteBufferDeleted { buffer_name } => {
                Some(buffer_name)
            }
            _ => None,
        }
    }

    /// Returns the best available current target for hook execution.
    #[must_use]
    pub fn current_target(&self) -> Option<Target> {
        match self {
            Self::WindowLinked {
                session_name,
                target: Some(target),
            }
            | Self::WindowUnlinked {
                session_name,
                target: Some(target),
                ..
            } => Some(Target::Window(WindowTarget::with_window(
                session_name.clone(),
                target.window_index(),
            ))),
            Self::WindowLinked { session_name, .. }
            | Self::WindowUnlinked { session_name, .. }
            | Self::ClientAttached { session_name, .. }
            | Self::ClientDetached { session_name, .. }
            | Self::ClientSessionChanged { session_name, .. }
            | Self::SessionCreated { session_name }
            | Self::SessionClosed { session_name, .. }
            | Self::SessionRenamed { session_name }
            | Self::SessionWindowChanged { session_name } => {
                Some(Target::Session(session_name.clone()))
            }
            Self::WindowRenamed { target }
            | Self::WindowLayoutChanged { target }
            | Self::WindowResized { target }
            | Self::WindowPaneChanged { target }
            | Self::AlertBell { target }
            | Self::AlertActivity { target }
            | Self::AlertSilence { target }
            | Self::AfterSelectWindow { target } => Some(Target::Window(target.clone())),
            Self::PaneExited { target, .. }
            | Self::PaneDied { target, .. }
            | Self::PaneModeChanged { target }
            | Self::AfterSelectPane { target }
            | Self::AfterSendKeys { target } => Some(Target::Pane(target.clone())),
            Self::AfterSetOption {
                session_name: Some(session_name),
            } => Some(Target::Session(session_name.clone())),
            Self::AfterSetOption { session_name: None }
            | Self::PasteBufferChanged { .. }
            | Self::PasteBufferDeleted { .. } => None,
        }
    }
}
