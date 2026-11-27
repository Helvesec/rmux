use rmux_core::{LifecycleEvent, WindowId};
use rmux_proto::{SessionId, WindowTarget};

use crate::handler::{
    prepare_unsequenced_lifecycle_event, sequence_prepared_lifecycle_event, RequestHandler,
    UnsequencedLifecycleEvent,
};
use crate::pane_terminals::HandlerState;

/// A first attach owns its resize publication until registration announces arrival.
/// Failed or cancelled upgrades still publish the geometry they already applied.
pub(crate) struct AttachResizePublication {
    handler: RequestHandler,
    events: Vec<UnsequencedLifecycleEvent>,
    window_identity: Option<(SessionId, WindowId)>,
    publication: Option<crate::lifecycle_commit_order::LifecyclePublicationGuard>,
}

impl AttachResizePublication {
    pub(super) fn capture(
        handler: &RequestHandler,
        state: &mut HandlerState,
        target: &WindowTarget,
    ) -> Option<Self> {
        let (target, layout_prepared, window_id) =
            state.take_applied_window_resize(target)?.into_parts();
        let window_identity = state
            .sessions
            .session(target.session_name())
            .map(|session| session.id())
            .zip(window_id);
        let mut events = Vec::new();
        if !layout_prepared {
            events.push(prepare_unsequenced_lifecycle_event(
                state,
                &LifecycleEvent::WindowLayoutChanged {
                    target: target.clone(),
                },
            ));
        }
        events.push(prepare_unsequenced_lifecycle_event(
            state,
            &LifecycleEvent::WindowResized { target },
        ));
        Some(Self {
            handler: handler.clone(),
            events,
            window_identity,
            publication: state.track_unordered_lifecycle_publication(),
        })
    }

    pub(crate) async fn publish(mut self) {
        let events = std::mem::take(&mut self.events);
        let handler = self.handler.clone();
        let publication = self.publication.take();
        let window_identity = self.window_identity;
        let task =
            tokio::spawn(
                async move { publish(&handler, events, window_identity, publication).await },
            );
        if let Err(error) = task.await {
            tracing::warn!(%error, "attach resize publication failed");
        }
    }
}

impl Drop for AttachResizePublication {
    fn drop(&mut self) {
        if self.events.is_empty() {
            return;
        }
        let handler = self.handler.clone();
        let events = std::mem::take(&mut self.events);
        let publication = self.publication.take();
        let window_identity = self.window_identity;
        tokio::spawn(async move { publish(&handler, events, window_identity, publication).await });
    }
}

async fn publish(
    handler: &RequestHandler,
    events: Vec<UnsequencedLifecycleEvent>,
    window_identity: Option<(SessionId, WindowId)>,
    _publication: Option<crate::lifecycle_commit_order::LifecyclePublicationGuard>,
) {
    let prepared = {
        let mut state = handler.state.lock().await;
        events
            .into_iter()
            .map(|event| {
                let mut event = sequence_prepared_lifecycle_event(&mut state, event);
                event.control_window_identity = window_identity;
                event
            })
            .collect::<Vec<_>>()
    };
    for event in prepared {
        handler.emit_prepared(event).await;
    }
}
