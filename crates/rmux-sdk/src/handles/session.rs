//! Daemon-backed session handle.

use std::fmt;
use std::time::Duration;

use crate::transport::{OperationDeadline, TransportClient};
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
        let transport = transport.with_default_timeout(
            crate::bootstrap::discovery::resolve_timeout(None, default_timeout),
        );
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

    pub(crate) fn begin_operation_handle(&self) -> Self {
        let mut session = Self {
            name: self.name.clone(),
            endpoint: self.endpoint.clone(),
            default_timeout: self.default_timeout,
            transport: self.transport.clone(),
            created: self.created,
            creation_tags: self.creation_tags.clone(),
        };
        session.transport = session.transport.begin_operation();
        session
    }

    pub(crate) fn with_operation_deadline(&self, deadline: OperationDeadline) -> Self {
        let mut session = self.begin_operation_handle();
        session.transport = session.transport.with_operation_deadline(deadline);
        session
    }

    pub(crate) const fn operation_deadline(&self) -> Option<OperationDeadline> {
        self.transport.operation_deadline()
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
        has_session(&self.transport.begin_operation(), self.name.clone()).await
    }

    /// Checks whether this session appears in the daemon's `list-sessions`
    /// projection.
    pub async fn is_listed(&self) -> Result<bool> {
        Ok(list_session_names(&self.transport.begin_operation())
            .await?
            .iter()
            .any(|candidate| candidate == &self.name))
    }

    /// Lists exact session names currently reported by the daemon.
    pub async fn list_session_names(&self) -> Result<Vec<SessionName>> {
        list_session_names(&self.transport.begin_operation()).await
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
        let transport = self.transport.begin_operation();
        let target = super::pane::resolve_pane_ref_for_id(&transport, &self.name, pane_id)
            .await?
            .ok_or_else(|| pane_not_found(&self.name, pane_id))?;
        Ok(Pane::new_by_id(
            target,
            pane_id,
            self.endpoint.clone(),
            self.default_timeout,
            transport,
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
        kill_session(&self.transport.begin_operation(), self.name.clone()).await
    }
}

fn pane_not_found(session_name: &SessionName, pane_id: PaneId) -> RmuxError {
    RmuxError::from_proto(rmux_proto::RmuxError::pane_not_found(
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
    let Some(target) = exact_session_target(&name) else {
        return Ok(list_session_names(client).await?.contains(&name));
    };
    match client
        .request(Request::HasSession(HasSessionRequest { target }))
        .await?
    {
        Response::HasSession(response) => Ok(response.exists),
        response => Err(unexpected_response("has-session", response)),
    }
}

pub(crate) async fn kill_session(client: &TransportClient, name: SessionName) -> Result<bool> {
    let target = match exact_session_target(&name) {
        Some(target) => target,
        None => session_id_target_named(client, &name)
            .await?
            .ok_or_else(|| session_not_found(&name))?,
    };
    match client
        .request(Request::KillSession(KillSessionRequest {
            target: target.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await
        .map_err(|error| restore_requested_name(error, &target, &name))?
    {
        Response::KillSession(response) => Ok(response.existed),
        response => Err(unexpected_response("kill-session", response)),
    }
}

/// Returns the daemon target that selects exactly the session called `name`.
///
/// A bare name is a tmux target: the daemon falls back to a unique prefix or
/// pattern, and names such as `~` or `{current}` select other sessions. The
/// `=` form disables every fallback, and a literal leading `=` survives
/// because the daemon strips only one.
///
/// Two kinds of names have no exact target form, so the callers resolve them
/// by listing instead:
/// - a name that starts with `$` remains a session-id target even after `=`;
/// - a canonical name that contains `\` (a literal backslash, or the escape of
///   a tab, control or bidi character) cannot be named by any target, because
///   the daemon escapes every backslash of the target again before its exact
///   comparison, so `=a\\b` looks for `a\\\\b`.
fn exact_session_target(name: &SessionName) -> Option<SessionName> {
    if name.as_str().starts_with('$') || name.as_str().contains('\\') {
        return None;
    }
    let target = SessionName::from_canonical(format!("={}", name.as_str()))
        .expect("a prefixed canonical session name is non-empty");
    debug_assert_eq!(&target.as_str()[1..], name.as_str());
    Some(target)
}

/// Resolves the `$N` identity of the session called exactly `name`.
///
/// Session ids are never reused by a daemon, so killing through the id cannot
/// reach a session created after the listing.
async fn session_id_target_named(
    client: &TransportClient,
    name: &SessionName,
) -> Result<Option<SessionName>> {
    let response = client
        .request(Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_id} #{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await?;
    let output = match response {
        Response::ListSessions(response) => response.output.stdout,
        response => return Err(unexpected_response("list-sessions", response)),
    };

    for line in String::from_utf8_lossy(&output).lines() {
        let Some((id, rendered_name)) = line.split_once(' ') else {
            continue;
        };
        let is_session_id = id
            .strip_prefix('$')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
        if is_session_id
            && SessionName::from_canonical(rendered_name).is_ok_and(|listed| &listed == name)
        {
            return Ok(Some(SessionName::from_canonical(id)?));
        }
    }
    Ok(None)
}

/// Reports a missing session under the name the caller asked for, not under
/// the exact target the SDK sent for it.
fn restore_requested_name(error: RmuxError, target: &SessionName, name: &SessionName) -> RmuxError {
    match error.protocol_source() {
        Some(rmux_proto::RmuxError::SessionNotFound(missing)) if missing == target.as_str() => {
            session_not_found(name)
        }
        _ => error,
    }
}

fn session_not_found(name: &SessionName) -> RmuxError {
    RmuxError::from_proto(rmux_proto::RmuxError::SessionNotFound(name.to_string()))
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
        .map(SessionName::from_canonical)
        .collect::<core::result::Result<Vec<_>, _>>()
        .map_err(RmuxError::from)
}

pub(crate) fn unexpected_response(expected: &'static str, response: Response) -> RmuxError {
    RmuxError::from_proto(rmux_proto::RmuxError::Server(format!(
        "rmux daemon sent `{}` response for `{expected}` request",
        response.command_name()
    )))
}

#[cfg(test)]
mod exact_target_tests {
    use super::{exact_session_target, SessionName};

    #[test]
    fn exact_target_adds_one_equals_sign_to_the_canonical_name() {
        for raw in ["work", "=work", "a=b", "~", "{current}"] {
            let name = SessionName::new(raw).expect("valid session name");
            let target =
                exact_session_target(&name).expect("only $ and backslash names lack a target");
            assert_eq!(target.as_str(), format!("={}", name.as_str()));
        }
    }

    #[test]
    fn dollar_names_have_no_exact_target_form() {
        for raw in ["$0", "$x", "$"] {
            let name = SessionName::new(raw).expect("valid session name");
            assert_eq!(exact_session_target(&name), None);
        }
    }

    #[test]
    fn backslash_names_have_no_exact_target_form() {
        for raw in [
            "back\\slash",
            "tab\there",
            "ctl\u{1}x",
            "bidi\u{202e}x",
            "\\",
        ] {
            let name = SessionName::new(raw).expect("valid session name");
            assert!(name.as_str().contains('\\'), "{raw:?} has a backslash");
            assert_eq!(exact_session_target(&name), None, "{raw:?}");
        }
    }
}
