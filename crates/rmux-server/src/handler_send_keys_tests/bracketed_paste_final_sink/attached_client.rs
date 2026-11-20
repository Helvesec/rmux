//! Attaches a final-sink test through the shared receiver-owning guard.
//!
//! [`AttachedFinalSinkClient`] lives in [`crate::test_shell::final_sink`] so
//! the deferred-start proof in `handler_session` attaches through the same
//! guard; only the focus precondition below is specific to this suite.

use super::*;

/// Declares focus reporting off on every pane of `session`, then attaches.
pub(super) async fn attach_to(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
) -> AttachedFinalSinkClient {
    // Every child here is asserted byte for byte at its final sink, including
    // both destinations when the panes are synchronized. Declare focus off for
    // each of them before the attach that would emit the first edge.
    let pane_indices = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(session)
            .expect("final-sink session exists before attaching")
            .window()
            .panes()
            .iter()
            .map(rmux_core::Pane::index)
            .collect::<Vec<_>>()
    };
    for pane_index in pane_indices {
        handler
            .disable_pane_focus_reports_for_test(&PaneTarget::with_window(
                session.clone(),
                0,
                pane_index,
            ))
            .await;
    }
    AttachedFinalSinkClient::register(handler, session).await
}
