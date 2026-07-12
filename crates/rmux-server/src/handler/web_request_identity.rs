use std::future::Future;

use rmux_proto::{
    PaneId, PaneTarget, Request, Response, RmuxError, SessionId, SessionName, WindowId,
    WindowTarget,
};

use crate::hook_runtime::PendingInlineHook;
use crate::pane_io::HandleOutcome;
use crate::pane_terminals::{HandlerState, WindowLinkOccurrenceId};

use super::RequestHandler;

#[derive(Clone)]
struct ExpectedSessionIdentity {
    name: SessionName,
    id: SessionId,
    window: Option<ExpectedWindowIdentity>,
}

#[derive(Clone, Copy)]
struct ExpectedWindowIdentity {
    index: u32,
    id: WindowId,
    occurrence_id: Option<WindowLinkOccurrenceId>,
}

#[derive(Clone)]
pub(in crate::handler) struct ExpectedWindowOccurrenceIdentity {
    name: SessionName,
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    occurrence_id: WindowLinkOccurrenceId,
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
        }
    }
}

tokio::task_local! {
    static EXPECTED_SESSION_IDENTITY: ExpectedSessionIdentity;
}

pub(in crate::handler) async fn with_expected_session_identity<T, F>(
    name: SessionName,
    id: SessionId,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    EXPECTED_SESSION_IDENTITY
        .scope(
            ExpectedSessionIdentity {
                name,
                id,
                window: None,
            },
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
    with_expected_window_identity_inner(name, session_id, window_index, window_id, None, future)
        .await
}

async fn with_expected_window_occurrence_identity<T, F>(
    name: SessionName,
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    occurrence_id: WindowLinkOccurrenceId,
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
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    EXPECTED_SESSION_IDENTITY
        .scope(
            ExpectedSessionIdentity {
                name,
                id: session_id,
                window: Some(ExpectedWindowIdentity {
                    index: window_index,
                    id: window_id,
                    occurrence_id,
                }),
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
    } = identity;
    let (outcome, inline_hooks) = with_expected_window_occurrence_identity(
        name,
        session_id,
        window_index,
        window_id,
        occurrence_id,
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
    require_expected_session_identity(state, target.session_name())?;
    let expected = EXPECTED_SESSION_IDENTITY.try_with(Clone::clone).ok();
    let Some(expected_window) = expected.and_then(|expected| expected.window) else {
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

pub(in crate::handler) fn resolve_expected_window_pane_target(
    state: &HandlerState,
    session_name: &SessionName,
    pane_id: PaneId,
) -> Result<Option<PaneTarget>, RmuxError> {
    let expected = EXPECTED_SESSION_IDENTITY.try_with(Clone::clone).ok();
    let Some(expected_window) = expected.and_then(|expected| expected.window) else {
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
    Ok(Some(PaneTarget::with_window(
        session_name.clone(),
        expected_window.index,
        pane_index,
    )))
}

pub(in crate::handler) fn require_expected_session_identity(
    state: &HandlerState,
    session_name: &SessionName,
) -> Result<(), RmuxError> {
    let expected = EXPECTED_SESSION_IDENTITY.try_with(Clone::clone).ok();
    let Some(expected) = expected else {
        return Ok(());
    };
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
