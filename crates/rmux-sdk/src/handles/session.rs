//! Daemon-backed session handle.

use std::fmt;
use std::time::Duration;

use crate::transport::TransportClient;
use crate::{PaneId, PaneRef, Result, RmuxEndpoint, RmuxError, SessionName, WindowRef};
use rmux_proto::{HasSessionRequest, KillSessionRequest, ListSessionsRequest, Request, Response};

use super::{Pane, Window};

/// Opaque handle for one live daemon session.
///
/// The handle stores the daemon transport and exact protocol-owned session
/// name. It never stores process environment supplied while creating the
/// session.
pub struct Session {
    name: SessionName,
    endpoint: RmuxEndpoint,
    default_timeout: Option<Duration>,
    transport: TransportClient,
    created: bool,
    creation_tags: Option<Vec<String>>,
}

impl Session {
    pub(crate) fn new(
        name: SessionName,
        endpoint: RmuxEndpoint,
        default_timeout: Option<Duration>,
        transport: TransportClient,
        created: bool,
        creation_tags: Option<Vec<String>>,
    ) -> Self {
        Self {
            name,
            endpoint,
            default_timeout,
            transport,
            created,
            creation_tags,
        }
    }

    /// Returns the exact protocol-owned session name addressed by this handle.
    #[must_use]
    pub fn name(&self) -> &SessionName {
        &self.name
    }

    /// Returns the endpoint that was resolved when this handle was created.
    #[must_use]
    pub fn endpoint(&self) -> &RmuxEndpoint {
        &self.endpoint
    }

    /// Returns the default timeout configured on the parent facade.
    #[must_use]
    pub const fn configured_default_timeout(&self) -> Option<Duration> {
        self.default_timeout
    }

    pub(crate) const fn transport(&self) -> &TransportClient {
        &self.transport
    }

    /// Returns whether the ensure operation created the session.
    ///
    /// `false` means the handle was bound to a session that already existed
    /// before the ensure request completed.
    #[must_use]
    pub const fn was_created(&self) -> bool {
        self.created
    }

    /// Returns caller-supplied creation tag intent, preserving explicit empty
    /// tag sets.
    #[must_use]
    pub fn creation_tags(&self) -> Option<&[String]> {
        self.creation_tags.as_deref()
    }

    /// Checks the live daemon for this session.
    pub async fn exists(&self) -> Result<bool> {
        has_session(&self.transport, self.name.clone()).await
    }

    /// Checks whether this session appears in the daemon's `list-sessions`
    /// projection.
    pub async fn is_listed(&self) -> Result<bool> {
        Ok(list_session_names(&self.transport)
            .await?
            .iter()
            .any(|candidate| candidate == &self.name))
    }

    /// Lists exact session names currently reported by the daemon.
    pub async fn list_session_names(&self) -> Result<Vec<SessionName>> {
        list_session_names(&self.transport).await
    }

    /// Returns a handle for a window slot in this session.
    ///
    /// The handle is intentionally lazy: it records the exact target and
    /// verifies liveness only when an operation such as `split`, `panes`,
    /// `info`, or `close` is invoked. Linked windows and grouped sessions are
    /// still resolved by the daemon on each operation rather than cached by the
    /// handle.
    #[must_use]
    pub fn window(&self, window_index: u32) -> Window {
        Window::new(
            WindowRef::new(self.name.clone(), window_index),
            self.endpoint.clone(),
            self.default_timeout,
            self.transport.clone(),
        )
    }

    /// Returns a handle for one pane slot inside a window of this session.
    ///
    /// The handle records the exact `(session, window, pane)` triple and
    /// resolves it through the daemon on every operation, so linked windows
    /// and grouped sessions keep returning the same stable pane identity
    /// across sibling views.
    #[must_use]
    pub fn pane(&self, window_index: u32, pane_index: u32) -> Pane {
        Pane::new(
            PaneRef::new(self.name.clone(), window_index, pane_index),
            self.endpoint.clone(),
            self.default_timeout,
            self.transport.clone(),
        )
    }

    /// Returns a pane handle addressed by stable pane id.
    ///
    /// The returned [`Pane`] has the same public type as a slot-based pane,
    /// but input, resize, lifecycle, title, and snapshot operations use the
    /// daemon's stable pane-id targeting path. `PaneId` is stable only for
    /// one daemon lifetime; callers that persist ids across reconnects must
    /// re-validate them.
    pub async fn pane_by_id(&self, pane_id: PaneId) -> Result<Pane> {
        let target = super::pane::resolve_pane_ref_for_id(&self.transport, &self.name, pane_id)
            .await?
            .ok_or_else(|| pane_not_found(&self.name, pane_id))?;
        Ok(Pane::new_by_id(
            target,
            pane_id,
            self.endpoint.clone(),
            self.default_timeout,
            self.transport.clone(),
        ))
    }

    /// Creates a new window in this session and returns a live window handle.
    ///
    /// This is an eager daemon mutation. Use [`Self::window`] when you only
    /// need a lazy handle for an existing or future window slot.
    pub async fn new_window(&self) -> Result<Window> {
        self.new_window_with().await
    }

    /// Starts building a configurable `new-window` request for this session.
    ///
    /// The default builder behavior matches tmux `new-window`: the created
    /// window becomes active unless [`NewWindowBuilder::detached`] is set.
    ///
    /// [`NewWindowBuilder::detached`]: crate::NewWindowBuilder::detached
    #[must_use]
    pub fn new_window_with(&self) -> crate::NewWindowBuilder<'_> {
        crate::NewWindowBuilder::new(self)
    }

    /// Starts a declarative SDK layout builder for this session.
    ///
    /// Layouts are SDK-side composition over the existing pane split,
    /// spawn, title, and daemon spread-layout primitives. They do not add a
    /// daemon-native transaction; if an intermediate split or spawn fails,
    /// already-created panes remain visible for inspection and cleanup by the
    /// caller.
    #[must_use]
    pub fn layout(&self) -> crate::SessionLayoutBuilder<'_> {
        crate::SessionLayoutBuilder::new(self)
    }

    /// Destroys this session through the daemon.
    ///
    /// The returned boolean mirrors the daemon response: `true` means a
    /// session existed and was removed.
    pub async fn kill(&self) -> Result<bool> {
        kill_session(&self.transport, self.name.clone()).await
    }
}

fn pane_not_found(session_name: &SessionName, pane_id: PaneId) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::pane_not_found(
        session_name.clone(),
        pane_id,
    ))
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("name", &self.name)
            .field("created", &self.created)
            .field("creation_tags", &self.creation_tags)
            .finish_non_exhaustive()
    }
}

pub(crate) async fn has_session(client: &TransportClient, name: SessionName) -> Result<bool> {
    match client
        .request(Request::HasSession(HasSessionRequest { target: name }))
        .await?
    {
        Response::HasSession(response) => Ok(response.exists),
        response => Err(unexpected_response("has-session", response)),
    }
}

pub(crate) async fn kill_session(client: &TransportClient, name: SessionName) -> Result<bool> {
    match client
        .request(Request::KillSession(KillSessionRequest {
            target: name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await?
    {
        Response::KillSession(response) => Ok(response.existed),
        response => Err(unexpected_response("kill-session", response)),
    }
}

pub(crate) async fn list_session_names(client: &TransportClient) -> Result<Vec<SessionName>> {
    let response = client
        .request(Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: Some("name".to_owned()),
            reversed: false,
        }))
        .await?;

    let output = match response {
        Response::ListSessions(response) => response.output.stdout,
        response => return Err(unexpected_response("list-sessions", response)),
    };

    String::from_utf8_lossy(&output)
        .lines()
        .map(SessionName::new)
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(RmuxError::protocol)
}

pub(crate) fn unexpected_response(expected: &'static str, response: Response) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "rmux daemon sent `{}` response for `{expected}` request",
        response.command_name()
    )))
}
