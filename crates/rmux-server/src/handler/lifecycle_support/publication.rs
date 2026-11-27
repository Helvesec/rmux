use tokio::sync::watch;

use super::*;

impl RequestHandler {
    pub(crate) async fn close_and_wait_for_lifecycle_publications(&self) {
        let pending = {
            let state = self.state.lock().await;
            state.close_lifecycle_commit_order()
        };
        pending.wait_until_idle().await;
    }

    pub(crate) async fn seal_and_wait_for_lifecycle_publications(&self) {
        let pending = {
            let state = self.state.lock().await;
            state.seal_lifecycle_publications()
        };
        pending.wait_until_idle().await;
    }

    #[cfg(test)]
    pub(crate) fn subscribe_lifecycle_events(&self) -> broadcast::Receiver<QueuedLifecycleEvent> {
        self.hook_events.subscribe()
    }

    pub(crate) fn take_lifecycle_dispatch_receiver(
        &self,
    ) -> Option<mpsc::Receiver<LifecycleDispatchItem>> {
        self.lifecycle_dispatch.activate()
    }

    pub(crate) fn deactivate_lifecycle_dispatch_for_shutdown(&self) {
        self.lifecycle_dispatch.deactivate();
    }

    #[cfg(test)]
    pub(crate) async fn consume_lifecycle_hooks(
        &self,
        events: mpsc::Receiver<LifecycleDispatchItem>,
        shutdown: oneshot::Receiver<()>,
    ) {
        let (unobserved_progress, _) = watch::channel(());
        self.consume_lifecycle_hooks_reporting_progress(events, shutdown, &unobserved_progress)
            .await;
    }

    /// Consumes lifecycle hooks and marks `progress` changed after each dispatched event,
    /// so a shutdown drain can tell a slow queue that advances from one that is stuck.
    pub(crate) async fn consume_lifecycle_hooks_reporting_progress(
        &self,
        mut events: mpsc::Receiver<LifecycleDispatchItem>,
        mut shutdown: oneshot::Receiver<()>,
        progress: &watch::Sender<()>,
    ) {
        loop {
            tokio::select! {
                result = events.recv() => {
                    match result {
                        Some(item) => {
                            self.dispatch_lifecycle_item(item).await;
                            progress.send_replace(());
                        }
                        None => break,
                    }
                }
                result = &mut shutdown => {
                    let _ = result;
                    self.lifecycle_dispatch.deactivate();
                    events.close();
                    while let Some(item) = events.recv().await {
                        self.dispatch_lifecycle_item(item).await;
                        progress.send_replace(());
                    }
                    self.shutdown_wait_for();
                    break;
                }
            }
        }
    }

    pub(in crate::handler) async fn emit(&self, event: LifecycleEvent) {
        if let LifecycleEvent::PaneModeChanged { target } = &event {
            self.refresh_automatic_window_name_for_pane_target(target)
                .await;
        }
        let queued = {
            let mut state = self.state.lock().await;
            prepare_lifecycle_event(&mut state, &event)
        };
        self.emit_prepared(queued).await;
    }

    #[cfg(test)]
    pub(crate) async fn emit_lifecycle_event_for_test(&self, event: LifecycleEvent) {
        self.emit(event).await;
    }

    #[cfg(test)]
    pub(crate) async fn prepare_lifecycle_event_for_test(
        &self,
        event: LifecycleEvent,
    ) -> QueuedLifecycleEvent {
        let mut state = self.state.lock().await;
        prepare_lifecycle_event(&mut state, &event)
    }

    #[cfg(test)]
    pub(crate) async fn emit_prepared_lifecycle_event_for_test(&self, event: QueuedLifecycleEvent) {
        self.emit_prepared(event).await;
    }

    pub(in crate::handler) async fn emit_without_attached_refresh(&self, event: LifecycleEvent) {
        let queued = {
            let mut state = self.state.lock().await;
            prepare_lifecycle_event(&mut state, &event)
        };
        self.emit_prepared(queued).await;
    }

    pub(in crate::handler) fn emit_prepared(
        &self,
        event: QueuedLifecycleEvent,
    ) -> impl Future<Output = ()> + Send {
        super::super::post_commit_sequencer::release_current_post_commit_turn();
        let handler = self.clone();
        let hook_execution = crate::hook_runtime::current_hook_execution();
        let hook_formats = crate::hook_runtime::current_hook_formats();
        let publication = tokio::spawn(async move {
            crate::hook_runtime::with_optional_hook_execution(
                hook_execution,
                hook_formats,
                handler.emit_prepared_inner(event),
            )
            .await;
        });
        async move {
            if let Err(error) = publication.await {
                warn!("durable lifecycle publication task failed: {error}");
            }
        }
    }

    pub(super) async fn emit_prepared_inner(&self, mut event: QueuedLifecycleEvent) {
        if event.publication_discarded {
            return;
        }
        let Some(ticket) = event.commit_order.as_ref() else {
            self.dispatch_lifecycle_control_effects_once(&mut event)
                .await;
            return;
        };
        let _commit_turn = ticket.wait_for_turn().await;
        self.dispatch_lifecycle_control_effects_once(&mut event)
            .await;
        let _ = self.hook_events.send(event.clone());
        let item = LifecycleDispatchItem {
            event,
            completion: None,
            _pending: self.lifecycle_dispatch.begin_pending(),
        };
        if self.lifecycle_dispatch.send_if_active(item).await.is_err() {
            warn!("lifecycle dispatch queue closed before server shutdown");
        }
    }

    pub(in crate::handler) fn emit_prepared_and_wait(
        &self,
        event: QueuedLifecycleEvent,
    ) -> impl Future<Output = ()> + Send {
        super::super::post_commit_sequencer::release_current_post_commit_turn();
        let handler = self.clone();
        let hook_execution = crate::hook_runtime::current_hook_execution();
        let hook_formats = crate::hook_runtime::current_hook_formats();
        let publication = tokio::spawn(async move {
            crate::hook_runtime::with_optional_hook_execution(
                hook_execution,
                hook_formats,
                handler.emit_prepared_and_wait_inner(event),
            )
            .await;
        });
        async move {
            if let Err(error) = publication.await {
                warn!("durable ordered lifecycle publication task failed: {error}");
            }
        }
    }

    pub(super) async fn emit_prepared_and_wait_inner(&self, mut event: QueuedLifecycleEvent) {
        if event.publication_discarded {
            return;
        }
        let Some(ticket) = event.commit_order.as_ref() else {
            self.dispatch_lifecycle_control_effects_once(&mut event)
                .await;
            return;
        };
        let commit_turn = ticket.wait_for_turn().await;
        self.dispatch_lifecycle_control_effects_once(&mut event)
            .await;
        let _ = self.hook_events.send(event.clone());
        let (completion_tx, completion_rx) = oneshot::channel();
        let item = LifecycleDispatchItem {
            event,
            completion: Some(completion_tx),
            _pending: self.lifecycle_dispatch.begin_pending(),
        };
        let admitted = match self.lifecycle_dispatch.send_if_active(item).await {
            Ok(admitted) => admitted,
            Err(_) => {
                warn!("lifecycle dispatch queue closed before ordered hook completed");
                false
            }
        };
        drop(commit_turn);
        if admitted {
            ordered_wait::wait_without_unbounded_caller_delay(completion_rx).await;
        }
    }

    pub(super) async fn dispatch_lifecycle_item(&self, item: LifecycleDispatchItem) {
        self.dispatch_lifecycle_hook(item.event).await;
        if let Some(completion) = item.completion {
            let _ = completion.send(());
        }
    }

    pub(crate) fn shutdown_wait_for(&self) {
        if let Ok(mut wait_for) = self.wait_for.lock() {
            wait_for.shutdown();
        }
    }

    pub(crate) fn flush_wait_for(&self) {
        if let Ok(mut wait_for) = self.wait_for.lock() {
            wait_for.flush();
        }
    }

    pub(crate) async fn emit_client_attached_identity(
        &self,
        client_name: String,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        self.emit_for_session_identity(
            LifecycleEvent::ClientAttached {
                session_name: session_name.clone(),
                client_name: Some(client_name),
            },
            &session_name,
            session_id,
        )
        .await;
    }

    /// Reports that the client named `client_name` moved to another session,
    /// then publishes the window geometry that move applied.
    ///
    /// tmux 3.7b, measured 2026-07-25 on both halves of `switch-client` with
    /// `window-size largest`: a control client watching the session a PTY client
    /// left receives `%client-session-changed` and then the source
    /// `%layout-change`, and a control client already on the destination
    /// receives `%client-session-changed` and then the destination
    /// `%layout-change @1 aefe,101x41,0,0,1 ...`. Draining the geometry queue
    /// here is what keeps every arm of every switch in that order.
    pub(crate) async fn emit_client_session_changed(
        &self,
        client_name: String,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        self.emit_for_session_identity(
            LifecycleEvent::ClientSessionChanged {
                session_name: session_name.clone(),
                client_name: Some(client_name),
            },
            &session_name,
            session_id,
        )
        .await;
        self.publish_applied_window_resizes().await;
    }

    pub(in crate::handler) async fn emit_for_session_identity(
        &self,
        event: LifecycleEvent,
        _session_name: &rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        let prepared = {
            let mut state = self.state.lock().await;
            let Some(session_name) = state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .find_map(|(session_name, session)| {
                    (session.id() == session_id).then(|| session_name.clone())
                })
            else {
                return;
            };
            let Some(event) = canonicalize_exact_session_event(event, session_name) else {
                debug_assert!(
                    false,
                    "exact session emission requires a client session event"
                );
                return;
            };
            let mut queued = prepare_lifecycle_event(&mut state, &event);
            queued.control_session_identity = Some(session_id);
            queued
        };
        self.emit_prepared(prepared).await;
    }
}
