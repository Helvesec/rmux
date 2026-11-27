use std::cell::RefCell;
use std::future::Future;

use rmux_proto::{Request, Response, SessionId, SessionName};

use super::queue::QueueExecutionContext;

tokio::task_local! {
    static QUEUED_NEW_SESSION_TRANSITION: RefCell<Option<QueuedCurrentSessionTransition>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueuedCurrentSessionTransition {
    session_id: SessionId,
    session_name: SessionName,
}

impl QueuedCurrentSessionTransition {
    pub(super) async fn capture<T, F>(
        context: &QueueExecutionContext,
        request: &Request,
        future: F,
    ) -> (T, Option<Self>)
    where
        F: Future<Output = T>,
    {
        if !context.accepts_explicit_current_session_transition()
            || !matches!(request, Request::NewSession(_) | Request::NewSessionExt(_))
        {
            return (future.await, None);
        }

        QUEUED_NEW_SESSION_TRANSITION
            .scope(RefCell::new(None), async move {
                let output = future.await;
                let transition =
                    QUEUED_NEW_SESSION_TRANSITION.with(|captured| captured.borrow().clone());
                (output, transition)
            })
            .await
    }

    pub(super) fn commit(self, response: &Response) -> Option<Self> {
        matches!(
            response,
            Response::NewSession(response) if response.session_name == self.session_name
        )
        .then_some(self)
    }

    pub(super) fn apply(&self, context: &mut QueueExecutionContext) {
        context
            .rebase_after_explicit_current_session_transition(self.session_id, &self.session_name);
    }
}

pub(in crate::handler) fn record_queued_new_session_transition(
    session_id: SessionId,
    session_name: SessionName,
) {
    let _ = QUEUED_NEW_SESSION_TRANSITION.try_with(|captured| {
        let mut captured = captured.borrow_mut();
        if captured.is_none() {
            *captured = Some(QueuedCurrentSessionTransition {
                session_id,
                session_name,
            });
        }
    });
}
