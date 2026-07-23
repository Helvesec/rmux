use std::cell::{Cell, RefCell};
use std::future::Future;

use rmux_proto::{
    PaneId, PaneTarget, Request, Response, RmuxError, SessionId, SessionName, Target, WindowId,
    WindowTarget,
};

use crate::hook_runtime::PendingInlineHook;
use crate::pane_io::HandleOutcome;
use crate::pane_terminals::{HandlerState, WindowLinkOccurrenceId};

use super::{
    attach_support::{ActiveAttachIdentity, AttachedSwitchCommittedTarget},
    client_support::SwitchManagedClientIdentity,
    RequestHandler,
};

#[cfg(test)]
#[path = "web_request_identity/test_support.rs"]
mod test_support;

struct ExpectedSessionIdentity {
    cursor: RefCell<ExpectedSessionCursor>,
    window: Option<ExpectedWindowIdentity>,
    policy: ExpectedSessionPolicy,
}

#[derive(Clone)]
struct ExpectedSessionCursor {
    name: SessionName,
    id: SessionId,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExpectedSessionPolicy {
    CapturedOnly,
    // Bindings may address another session explicitly, but their implicit
    // attached-session cursor remains identity guarded and switch-rebased.
    AttachedCommandQueue,
}

#[derive(Clone, Copy)]
struct ExpectedWindowIdentity {
    index: u32,
    id: WindowId,
    occurrence_id: Option<WindowLinkOccurrenceId>,
    pane_output_generation: Option<(PaneId, u64)>,
}

#[derive(Clone)]
pub(in crate::handler) struct ExpectedWindowOccurrenceIdentity {
    name: SessionName,
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    occurrence_id: WindowLinkOccurrenceId,
    pane_output_generation: Option<(PaneId, u64)>,
}

impl ExpectedWindowOccurrenceIdentity {
    pub(in crate::handler) fn new(
        name: SessionName,
        session_id: SessionId,
        window_index: u32,
        window_id: WindowId,
        occurrence_id: WindowLinkOccurrenceId,
    ) -> Self {
        Self {
            name,
            session_id,
            window_index,
            window_id,
            occurrence_id,
            pane_output_generation: None,
        }
    }

    pub(in crate::handler) fn with_pane_output_generation(
        mut self,
        pane_id: PaneId,
        output_generation: u64,
    ) -> Self {
        self.pane_output_generation = Some((pane_id, output_generation));
        self
    }
}

tokio::task_local! {
    static EXPECTED_SESSION_IDENTITY: ExpectedSessionIdentity;
    static EXPECTED_ATTACH_IDENTITY: ExpectedAttachIdentity;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttachRegistrationIdentity {
    pid: u32,
    id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedAttachPolicy {
    SessionCursor,
    RegistrationOnly,
}

// Queue ownership follows the registration, while the attached session is a
// cursor that may advance after a successful switch-client.
struct ExpectedAttachIdentity {
    registration: AttachRegistrationIdentity,
    session_id: Cell<SessionId>,
    policy: ExpectedAttachPolicy,
}

impl ExpectedAttachIdentity {
    fn new(identity: ActiveAttachIdentity) -> Self {
        Self {
            registration: AttachRegistrationIdentity {
                pid: identity.attach_pid(),
                id: identity.attach_id(),
            },
            session_id: Cell::new(identity.session_id()),
            policy: ExpectedAttachPolicy::SessionCursor,
        }
    }

    fn registration_only(identity: ActiveAttachIdentity) -> Self {
        let mut expected = Self::new(identity);
        expected.policy = ExpectedAttachPolicy::RegistrationOnly;
        expected
    }

    fn snapshot(&self) -> ActiveAttachIdentity {
        ActiveAttachIdentity::new(
            self.registration.pid,
            self.registration.id,
            self.session_id.get(),
        )
    }
}

pub(in crate::handler) fn current_expected_attach_identity() -> Option<ActiveAttachIdentity> {
    EXPECTED_ATTACH_IDENTITY
        .try_with(ExpectedAttachIdentity::snapshot)
        .ok()
}

pub(in crate::handler) fn expected_attach_follows_registration() -> bool {
    EXPECTED_ATTACH_IDENTITY
        .try_with(|expected| expected.policy == ExpectedAttachPolicy::RegistrationOnly)
        .unwrap_or(false)
}

pub(in crate::handler) async fn validate_expected_attach_identity(
    handler: &RequestHandler,
    requester_pid: u32,
) -> Result<Option<ActiveAttachIdentity>, RmuxError> {
    let Some((identity, policy)) = EXPECTED_ATTACH_IDENTITY
        .try_with(|expected| (expected.snapshot(), expected.policy))
        .ok()
    else {
        return Ok(None);
    };
    let current_identity = if identity.attach_pid() == requester_pid {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&requester_pid)
            .filter(|active| {
                active.id == identity.attach_id()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .map(|active| ActiveAttachIdentity::new(requester_pid, active.id, active.session_id))
    } else {
        None
    };
    let Some(current_identity) = current_identity else {
        return Err(changed_attach_identity_error());
    };
    if policy == ExpectedAttachPolicy::SessionCursor
        && current_identity.session_id() != identity.session_id()
    {
        return Err(changed_attach_identity_error());
    }
    if policy == ExpectedAttachPolicy::RegistrationOnly {
        EXPECTED_ATTACH_IDENTITY
            .try_with(|expected| expected.session_id.set(current_identity.session_id()))
            .map_err(|_| changed_attach_identity_error())?;
    }
    Ok(Some(current_identity))
}

pub(in crate::handler) async fn with_expected_attach_identity<T, F>(
    identity: ActiveAttachIdentity,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    EXPECTED_ATTACH_IDENTITY
        .scope(ExpectedAttachIdentity::new(identity), future)
        .await
}

pub(in crate::handler) async fn with_expected_attach_registration<T, F>(
    identity: ActiveAttachIdentity,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    EXPECTED_ATTACH_IDENTITY
        .scope(ExpectedAttachIdentity::registration_only(identity), future)
        .await
}

pub(in crate::handler) async fn with_expected_attach_and_session_identity<T, F>(
    identity: ActiveAttachIdentity,
    name: SessionName,
    session_id: SessionId,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let identity =
        ActiveAttachIdentity::new(identity.attach_pid(), identity.attach_id(), session_id);
    with_expected_attach_identity(
        identity,
        with_expected_session_identity_inner(
            name,
            session_id,
            None,
            ExpectedSessionPolicy::AttachedCommandQueue,
            future,
        ),
    )
    .await
}

pub(in crate::handler) async fn rebase_expected_attach_session_after_switch(
    handler: &RequestHandler,
    requester_pid: u32,
    targeted_client: SwitchManagedClientIdentity,
    response_session: &SessionName,
    committed_target: Option<AttachedSwitchCommittedTarget>,
) -> Result<Option<Target>, RmuxError> {
    let Some(expected) = current_expected_attach_identity() else {
        return Ok(None);
    };
    if expected.attach_pid() != requester_pid {
        return Err(changed_attach_identity_error());
    }
    let expected_registration = AttachRegistrationIdentity {
        pid: requester_pid,
        id: expected.attach_id(),
    };
    let targets_requester = targeted_client
        == (SwitchManagedClientIdentity::Attach {
            pid: requester_pid,
            attach_id: expected.attach_id(),
        });
    if matches!(
        targeted_client,
        SwitchManagedClientIdentity::Attach { pid, attach_id }
            if pid == requester_pid && attach_id != expected.attach_id()
    ) {
        return Err(changed_attach_identity_error());
    }
    if !targets_requester {
        return Ok(None);
    }
    let Some(committed_target) = committed_target else {
        return Ok(None);
    };
    #[cfg(test)]
    test_support::pause_before_attached_queue_switch_response_correlation(
        expected_registration.pid,
        expected_registration.id,
    )
    .await;

    let state = handler.state.lock().await;
    let active_attach = handler.active_attach.lock().await;
    let Some(active) = active_attach.by_pid.get(&requester_pid).filter(|active| {
        active.id == expected.attach_id()
            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            && &active.session_name == response_session
            && state
                .sessions
                .session(response_session)
                .is_some_and(|session| session.id() == active.session_id)
    }) else {
        return Err(switch_response_identity_error());
    };
    if committed_target.target.session_name() != response_session {
        return Err(switch_response_identity_error());
    }
    let session = state
        .sessions
        .session(response_session)
        .filter(|session| {
            session.id() == active.session_id && session.id() == committed_target.session_id
        })
        .ok_or_else(switch_response_identity_error)?;
    let committed_identity_exists = session
        .window_at(committed_target.target.window_index())
        .filter(|window| window.id() == committed_target.window_id)
        .and_then(|window| window.pane(committed_target.target.pane_index()))
        .is_some_and(|pane| pane.id() == committed_target.pane_id);
    if !committed_identity_exists {
        return Err(switch_response_identity_error());
    }
    let session_id = active.session_id;
    EXPECTED_ATTACH_IDENTITY
        .try_with(|identity| {
            if identity.registration != expected_registration {
                return Err(changed_attach_identity_error());
            }
            identity.session_id.set(session_id);
            Ok(())
        })
        .map_err(|_| changed_attach_identity_error())??;
    let _ = EXPECTED_SESSION_IDENTITY.try_with(|identity| {
        if identity.policy == ExpectedSessionPolicy::AttachedCommandQueue {
            *identity.cursor.borrow_mut() = ExpectedSessionCursor {
                name: response_session.clone(),
                id: session_id,
            };
        }
    });
    Ok(Some(Target::Pane(committed_target.target)))
}

fn changed_attach_identity_error() -> RmuxError {
    RmuxError::Server("attached client identity changed before queued command execution".to_owned())
}

fn switch_response_identity_error() -> RmuxError {
    RmuxError::Server(
        "switch-client response no longer matches the targeted client identity".to_owned(),
    )
}

pub(in crate::handler) async fn with_expected_session_identity<T, F>(
    name: SessionName,
    id: SessionId,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    with_expected_session_identity_inner(
        name,
        id,
        None,
        ExpectedSessionPolicy::CapturedOnly,
        future,
    )
    .await
}

pub(in crate::handler) async fn with_expected_window_identity<T, F>(
    name: SessionName,
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    with_expected_window_identity_inner(
        name,
        session_id,
        window_index,
        window_id,
        None,
        None,
        future,
    )
    .await
}

async fn with_expected_window_occurrence_identity<T, F>(
    name: SessionName,
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    occurrence_id: WindowLinkOccurrenceId,
    pane_output_generation: Option<(PaneId, u64)>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    with_expected_window_identity_inner(
        name,
        session_id,
        window_index,
        window_id,
        Some(occurrence_id),
        pane_output_generation,
        future,
    )
    .await
}

async fn with_expected_window_identity_inner<T, F>(
    name: SessionName,
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    occurrence_id: Option<WindowLinkOccurrenceId>,
    pane_output_generation: Option<(PaneId, u64)>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    with_expected_session_identity_inner(
        name,
        session_id,
        Some(ExpectedWindowIdentity {
            index: window_index,
            id: window_id,
            occurrence_id,
            pane_output_generation,
        }),
        ExpectedSessionPolicy::CapturedOnly,
        future,
    )
    .await
}

async fn with_expected_session_identity_inner<T, F>(
    name: SessionName,
    id: SessionId,
    window: Option<ExpectedWindowIdentity>,
    policy: ExpectedSessionPolicy,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    EXPECTED_SESSION_IDENTITY
        .scope(
            ExpectedSessionIdentity {
                cursor: RefCell::new(ExpectedSessionCursor { name, id }),
                window,
                policy,
            },
            future,
        )
        .await
}

pub(in crate::handler) async fn dispatch_with_expected_session_identity(
    handler: &RequestHandler,
    requester_pid: u32,
    name: SessionName,
    id: SessionId,
    request: Request,
) -> Response {
    let request_for_hooks = request.clone();
    let (outcome, inline_hooks) = with_expected_session_identity(
        name,
        id,
        handler.dispatch_captured(requester_pid, u64::from(requester_pid), request),
    )
    .await;
    finish_identity_dispatch(
        handler,
        requester_pid,
        request_for_hooks,
        outcome,
        inline_hooks,
    )
    .await
}

pub(in crate::handler) async fn dispatch_with_expected_window_identity(
    handler: &RequestHandler,
    requester_pid: u32,
    name: SessionName,
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    request: Request,
) -> Response {
    let request_for_hooks = request.clone();
    let (outcome, inline_hooks) = with_expected_window_identity(
        name,
        session_id,
        window_index,
        window_id,
        handler.dispatch_captured(requester_pid, u64::from(requester_pid), request),
    )
    .await;
    finish_identity_dispatch(
        handler,
        requester_pid,
        request_for_hooks,
        outcome,
        inline_hooks,
    )
    .await
}

pub(in crate::handler) async fn dispatch_with_expected_window_occurrence_identity(
    handler: &RequestHandler,
    requester_pid: u32,
    identity: ExpectedWindowOccurrenceIdentity,
    request: Request,
) -> Response {
    let request_for_hooks = request.clone();
    let ExpectedWindowOccurrenceIdentity {
        name,
        session_id,
        window_index,
        window_id,
        occurrence_id,
        pane_output_generation,
    } = identity;
    let (outcome, inline_hooks) = with_expected_window_occurrence_identity(
        name,
        session_id,
        window_index,
        window_id,
        occurrence_id,
        pane_output_generation,
        handler.dispatch_captured(requester_pid, u64::from(requester_pid), request),
    )
    .await;
    finish_identity_dispatch(
        handler,
        requester_pid,
        request_for_hooks,
        outcome,
        inline_hooks,
    )
    .await
}

async fn finish_identity_dispatch(
    handler: &RequestHandler,
    requester_pid: u32,
    request: Request,
    outcome: HandleOutcome,
    inline_hooks: Vec<PendingInlineHook>,
) -> Response {
    let inline_hook_names = inline_hooks
        .iter()
        .map(|pending| pending.hook)
        .collect::<Vec<_>>();
    handler
        .run_inline_hooks(requester_pid, inline_hooks, None)
        .await;
    handler
        .run_request_hooks(
            requester_pid,
            &request,
            &outcome.response,
            None,
            &inline_hook_names,
        )
        .await;
    outcome.response
}

pub(in crate::handler) fn require_expected_window_identity(
    state: &HandlerState,
    target: &WindowTarget,
) -> Result<(), RmuxError> {
    super::require_expected_stable_window_identity(state, target)?;
    require_expected_session_identity(state, target.session_name())?;
    let expected_window = EXPECTED_SESSION_IDENTITY
        .try_with(|expected| expected.window)
        .ok()
        .flatten();
    let Some(expected_window) = expected_window else {
        return Ok(());
    };
    let matches = expected_window.index == target.window_index()
        && state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some_and(|window| window.id() == expected_window.id)
        && expected_window.occurrence_id.is_none_or(|occurrence_id| {
            state.window_link_occurrence_id(target.session_name(), target.window_index())
                == Some(occurrence_id)
        });
    if matches {
        Ok(())
    } else {
        Err(RmuxError::invalid_target(
            target.to_string(),
            "window identity changed before mutation",
        ))
    }
}

pub(in crate::handler) fn require_expected_pane_identity(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    super::require_expected_stable_pane_identity(state, target)?;
    require_expected_window_identity(
        state,
        &WindowTarget::with_window(target.session_name().clone(), target.window_index()),
    )
}

pub(in crate::handler) fn resolve_expected_window_pane_target(
    state: &HandlerState,
    session_name: &SessionName,
    pane_id: PaneId,
) -> Result<Option<PaneTarget>, RmuxError> {
    let expected_window = EXPECTED_SESSION_IDENTITY
        .try_with(|expected| expected.window)
        .ok()
        .flatten();
    let Some(expected_window) = expected_window else {
        return Ok(None);
    };
    let window_target = WindowTarget::with_window(session_name.clone(), expected_window.index);
    require_expected_window_identity(state, &window_target)?;
    let window = state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(expected_window.index))
        .expect("expected window identity was already validated");
    let pane_index = window
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)
        .map(rmux_core::Pane::index)
        .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), pane_id))?;
    let target = PaneTarget::with_window(session_name.clone(), expected_window.index, pane_index);
    if let Some((expected_pane_id, expected_generation)) = expected_window.pane_output_generation {
        let generation_matches = expected_pane_id == pane_id
            && state.pane_output_generation_for_target(&target, pane_id) == expected_generation;
        if !generation_matches {
            return Err(RmuxError::invalid_target(
                target.to_string(),
                "pane output generation changed before mutation",
            ));
        }
    }
    Ok(Some(target))
}

pub(in crate::handler) fn require_expected_session_identity(
    state: &HandlerState,
    session_name: &SessionName,
) -> Result<(), RmuxError> {
    super::require_expected_stable_session_identity(state, session_name)?;
    let expected = EXPECTED_SESSION_IDENTITY
        .try_with(|expected| (expected.cursor.borrow().clone(), expected.policy))
        .ok();
    let Some((expected, policy)) = expected else {
        return Ok(());
    };
    if policy == ExpectedSessionPolicy::AttachedCommandQueue && expected.name != *session_name {
        return Ok(());
    }
    let matches = expected.name == *session_name
        && state
            .sessions
            .session(session_name)
            .is_some_and(|session| session.id() == expected.id);
    if matches {
        Ok(())
    } else {
        Err(RmuxError::SessionNotFound(session_name.to_string()))
    }
}
