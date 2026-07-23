use rmux_core::{LifecycleEvent, SetBufferOutcome};
use rmux_proto::RmuxError;

use super::capture_format::parse_buffer_limit;
use crate::handler::{QueuedLifecycleEvent, RequestHandler};
use crate::pane_terminals::HandlerState;

impl RequestHandler {
    pub(in crate::handler) async fn store_buffer(
        &self,
        name: Option<String>,
        content: Vec<u8>,
    ) -> Result<String, RmuxError> {
        self.store_buffer_mutation(name, content, false, false)
            .await
            .map(|(name, _)| name)
    }

    pub(in crate::handler) async fn store_buffer_with_append(
        &self,
        name: Option<String>,
        content: Vec<u8>,
        append: bool,
    ) -> Result<(String, Vec<u8>), RmuxError> {
        self.store_buffer_mutation(name, content, append, true)
            .await
            .map(|(name, content)| {
                (
                    name,
                    content.expect("buffer snapshot was requested for append/store"),
                )
            })
    }

    async fn store_buffer_mutation(
        &self,
        name: Option<String>,
        mut content: Vec<u8>,
        append: bool,
        snapshot_content: bool,
    ) -> Result<(String, Option<Vec<u8>>), RmuxError> {
        let capacity = self.pane_mode_post_commit.acquire_capacity().await?;
        let post_commit = capacity.sequence();
        let handler = self.clone();
        post_commit
            .run_durable(async move {
                let transaction = handler.pane_mode_transaction.clone().lock_owned().await;
                let (buffer_name, content_snapshot, events) = {
                    let mut state = handler.state.lock().await;
                    if append {
                        if let Some(existing) = name
                            .as_deref()
                            .and_then(|buffer_name| state.buffers.get(buffer_name))
                        {
                            let mut combined = Vec::with_capacity(existing.len() + content.len());
                            combined.extend_from_slice(existing);
                            combined.append(&mut content);
                            content = combined;
                        }
                    }
                    let content_snapshot = snapshot_content.then(|| content.clone());
                    let outcome =
                        Self::store_buffer_in_state(&mut state, name.as_deref(), content)?;
                    let (buffer_name, events) =
                        Self::prepare_buffer_store_outcome_in_state(&mut state, &outcome);
                    (buffer_name, content_snapshot, events)
                };
                drop(transaction);
                for event in events {
                    handler.emit_prepared(event).await;
                }
                Ok::<_, RmuxError>((buffer_name, content_snapshot))
            })
            .await?
    }

    pub(in crate::handler) fn store_buffer_in_state(
        state: &mut HandlerState,
        name: Option<&str>,
        content: Vec<u8>,
    ) -> Result<SetBufferOutcome, RmuxError> {
        let buffer_limit = parse_buffer_limit(state);
        state.buffers.set(name, content, buffer_limit)
    }

    pub(in crate::handler) fn prepare_buffer_store_outcome_in_state(
        state: &mut HandlerState,
        outcome: &SetBufferOutcome,
    ) -> (String, Vec<QueuedLifecycleEvent>) {
        let buffer_name = outcome.buffer_name().map(str::to_owned).unwrap_or_default();
        let mut lifecycle_events = outcome
            .evicted()
            .iter()
            .cloned()
            .map(|buffer_name| LifecycleEvent::PasteBufferDeleted { buffer_name })
            .collect::<Vec<_>>();
        if !buffer_name.is_empty() {
            lifecycle_events.push(LifecycleEvent::PasteBufferChanged {
                buffer_name: buffer_name.clone(),
            });
        }
        let events = lifecycle_events
            .iter()
            .map(|event| super::super::prepare_lifecycle_event(state, event))
            .collect();
        (buffer_name, events)
    }
}
