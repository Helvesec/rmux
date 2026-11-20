use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{PaneId, PaneRef, SessionName};

/// The pane or session exposed by a web share, as reported by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebShareScope {
    /// Expose exactly one pane, selected by slot or stable identity.
    Pane(WebSharePaneTarget),
    /// Expose an attached-client view of one session.
    Session(SessionName),
}

impl WebShareScope {
    /// Returns true when this share exposes one pane.
    #[must_use]
    pub const fn is_pane(&self) -> bool {
        matches!(self, Self::Pane(_))
    }

    /// Returns true when this share exposes one session.
    #[must_use]
    pub const fn is_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }

    pub(super) fn from_proto(value: rmux_proto::WebShareScope) -> Self {
        match value {
            rmux_proto::WebShareScope::Pane(target) => {
                Self::Pane(WebSharePaneTarget::from_proto(target))
            }
            rmux_proto::WebShareScope::Session(name) => Self::Session(name),
        }
    }
}

impl fmt::Display for WebShareScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pane(target) => target.fmt(formatter),
            Self::Session(name) => name.fmt(formatter),
        }
    }
}

/// An exact pane selector reported by a web share.
///
/// Slots and stable IDs remain distinct. These values do not resolve a target
/// or reconstruct a session name from its display text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WebSharePaneTarget {
    /// A session, window index and pane index.
    Slot(PaneRef),
    /// A stable pane identity within one daemon lifetime, scoped by session.
    Id {
        /// Exact session name component.
        session_name: SessionName,
        /// Stable pane identity.
        pane_id: PaneId,
    },
}

impl WebSharePaneTarget {
    /// Creates an indexed selector.
    #[must_use]
    pub const fn slot(target: PaneRef) -> Self {
        Self::Slot(target)
    }

    /// Creates a stable-ID selector.
    #[must_use]
    pub const fn by_id(session_name: SessionName, pane_id: PaneId) -> Self {
        Self::Id {
            session_name,
            pane_id,
        }
    }

    /// Returns the exact session name component.
    #[must_use]
    pub const fn session_name(&self) -> &SessionName {
        match self {
            Self::Slot(target) => &target.session_name,
            Self::Id { session_name, .. } => session_name,
        }
    }

    /// Returns the stable pane identity only for an ID selector.
    #[must_use]
    pub const fn pane_id(&self) -> Option<PaneId> {
        match self {
            Self::Slot(_) => None,
            Self::Id { pane_id, .. } => Some(*pane_id),
        }
    }

    pub(super) fn from_proto(value: rmux_proto::PaneTargetRef) -> Self {
        match value {
            rmux_proto::PaneTargetRef::Slot(target) => Self::Slot(PaneRef::from_proto(target)),
            rmux_proto::PaneTargetRef::Id {
                session_name,
                pane_id,
            } => Self::Id {
                session_name,
                pane_id,
            },
        }
    }
}

impl From<PaneRef> for WebSharePaneTarget {
    fn from(value: PaneRef) -> Self {
        Self::Slot(value)
    }
}

impl fmt::Display for WebSharePaneTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slot(target) => target.fmt(formatter),
            Self::Id {
                session_name,
                pane_id,
            } => write!(formatter, "{session_name}:{pane_id}"),
        }
    }
}
