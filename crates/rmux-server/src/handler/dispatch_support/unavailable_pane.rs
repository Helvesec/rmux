//! Request boundary for a pane whose transcript failed fast on poison.
//!
//! [`SharedPaneTranscript::lock`](crate::pane_transcript::SharedPaneTranscript::lock)
//! keeps aborting the operation that touches a poisoned transcript. Only its
//! typed payload is answered here, as an error to the requesting client, so the
//! connection and its later requests stay served. Any other panic keeps
//! unwinding unchanged. Only under this boundary does a format fail fast on
//! the poisoned pane it names.

use std::future::Future;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll};

use rmux_proto::{ErrorResponse, Response, RmuxError};
use tracing::warn;

use crate::pane_io::HandleOutcome;
use crate::pane_transcript::{with_unavailable_pane_answered, PaneTranscriptUnavailable};

/// Polls one dispatched request and answers a poisoned-pane abort.
pub(super) struct AnswerUnavailablePane<F> {
    dispatch: Option<F>,
}

pub(super) fn answer_unavailable_pane<F>(dispatch: F) -> impl Future<Output = HandleOutcome>
where
    F: Future<Output = HandleOutcome> + Unpin,
{
    with_unavailable_pane_answered(AnswerUnavailablePane {
        dispatch: Some(dispatch),
    })
}

impl<F> Future for AnswerUnavailablePane<F>
where
    F: Future<Output = HandleOutcome> + Unpin,
{
    type Output = HandleOutcome;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<HandleOutcome> {
        let dispatch = self
            .dispatch
            .as_mut()
            .expect("an answered dispatch is not polled again");
        // The request's own state is not reused after an abort: the future
        // is dropped below, as the connection task's unwinding would.
        let polled = catch_unwind(AssertUnwindSafe(|| Pin::new(dispatch).poll(cx)));
        match polled {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(outcome)) => {
                self.dispatch = None;
                Poll::Ready(outcome)
            }
            Err(payload) => {
                self.dispatch = None;
                match payload.downcast::<PaneTranscriptUnavailable>() {
                    Ok(unavailable) => Poll::Ready(unavailable_pane_outcome(*unavailable)),
                    Err(payload) => resume_unwind(payload),
                }
            }
        }
    }
}

fn unavailable_pane_outcome(unavailable: PaneTranscriptUnavailable) -> HandleOutcome {
    warn!(
        pane = %unavailable.pane_id(),
        "request aborted on a poisoned pane transcript"
    );
    HandleOutcome::response(Response::Error(ErrorResponse {
        error: RmuxError::Message(unavailable.to_string()),
    }))
}
