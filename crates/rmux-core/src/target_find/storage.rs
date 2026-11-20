//! The two storage forms accepted by the target resolver. All window access
//! borrows the original owner; iteration retains each session-local occurrence.
use crate::{
    ManagedSessionStore, ManagedSessionView, Session, SessionId, SessionName, SessionRecency,
    SessionStore, Window,
};
use rmux_proto::RmuxError;

pub(super) enum TargetStore<'a> {
    Standalone(&'a SessionStore),
    Managed(&'a ManagedSessionStore),
}

#[derive(Clone, Copy)]
pub(super) enum TargetSession<'a> {
    Standalone(&'a Session),
    Managed(ManagedSessionView<'a>),
}

impl<'a> TargetStore<'a> {
    pub(super) fn session(&self, name: &SessionName) -> Option<TargetSession<'a>> {
        match self {
            Self::Standalone(store) => store.session(name).map(TargetSession::Standalone),
            Self::Managed(store) => store.session(name).map(TargetSession::Managed),
        }
    }

    pub(super) fn session_by_id(&self, id: SessionId) -> Option<TargetSession<'a>> {
        match self {
            Self::Standalone(store) => store.session_by_id(id).map(TargetSession::Standalone),
            Self::Managed(store) => store.session_by_id(id).map(TargetSession::Managed),
        }
    }

    pub(super) fn iter(
        &self,
    ) -> Box<dyn Iterator<Item = (&'a SessionName, TargetSession<'a>)> + 'a> {
        match self {
            Self::Standalone(store) => Box::new(
                store
                    .iter()
                    .map(|(name, session)| (name, TargetSession::Standalone(session))),
            ),
            Self::Managed(store) => Box::new(
                store
                    .iter()
                    .map(|session| (session.name(), TargetSession::Managed(session))),
            ),
        }
    }
}

impl<'a> TargetSession<'a> {
    pub(super) fn name(self) -> &'a SessionName {
        match self {
            Self::Standalone(session) => session.name(),
            Self::Managed(session) => session.name(),
        }
    }

    pub(super) fn active_window_index(self) -> u32 {
        match self {
            Self::Standalone(session) => session.active_window_index(),
            Self::Managed(session) => session.active_window_index(),
        }
    }

    pub(super) fn last_window_index(self) -> Option<u32> {
        match self {
            Self::Standalone(session) => session.last_window_index(),
            Self::Managed(session) => session.last_window_index(),
        }
    }

    pub(super) fn last_attached_at(self) -> Option<i64> {
        match self {
            Self::Standalone(session) => session.last_attached_at(),
            Self::Managed(session) => session.last_attached_at(),
        }
    }

    pub(super) fn recency(self) -> SessionRecency {
        match self {
            Self::Standalone(session) => session.recency(),
            Self::Managed(session) => session.recency(),
        }
    }

    pub(super) fn window_at(self, index: u32) -> Option<&'a Window> {
        match self {
            Self::Standalone(session) => session.window_at(index),
            Self::Managed(session) => session.window_at(index),
        }
    }

    pub(super) fn windows(self) -> Box<dyn DoubleEndedIterator<Item = (u32, &'a Window)> + 'a> {
        match self {
            Self::Standalone(session) => Box::new(session.windows().iter().map(|(i, w)| (*i, w))),
            Self::Managed(session) => Box::new(session.windows()),
        }
    }

    pub(super) fn lowest_available_window_index_at_or_above(
        self,
        minimum: u32,
    ) -> Result<u32, RmuxError> {
        match self {
            Self::Standalone(session) => session.lowest_available_window_index_at_or_above(minimum),
            Self::Managed(session) => session.lowest_available_window_index_at_or_above(minimum),
        }
    }
}
