use std::time::Duration;

use rmux_core::events::{OutputCursorItem, PaneOutputSubscriptionKey};
use rmux_core::LifecycleEvent;
use rmux_proto::{OptionName, PaneTarget, RmuxError, SessionId, Target, WindowTarget};

use super::super::{
    prepare_lifecycle_event, scripting_support::format_context_for_target, RequestHandler,
};
use crate::format_runtime::render_runtime_template;
use crate::pane_io::{PaneExitCallback, PaneExitEvent, PaneOutputReceiver, PaneOutputSender};
use crate::pane_terminal_lookup::missing_pane_terminal;
use crate::pane_terminals::{session_not_found, HandlerState, PaneExitMetadata};
use tracing::warn;

const PANE_EXIT_STATUS_RETRY_DELAY: Duration = Duration::from_millis(10);
const PANE_EXIT_STATUS_RETRY_ATTEMPTS: usize = 20;
const DEAD_PANE_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

enum PaneExitPlan {
    Ignore,
    KeepDead {
        runtime_session_name: rmux_proto::SessionName,
        target: PaneTarget,
        prepare_dead: bool,
        pane_event: super::super::QueuedLifecycleEvent,
        output: ExitedPaneOutput,
    },
    RemovePane {
        runtime_session_name: rmux_proto::SessionName,
        session_name: rmux_proto::SessionName,
        target: PaneTarget,
        window_destroyed: bool,
        removed_pane_ids: Vec<rmux_core::PaneId>,
        pane_event: super::super::QueuedLifecycleEvent,
        output: ExitedPaneOutput,
    },
    RemoveSession {
        runtime_session_name: rmux_proto::SessionName,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
        target: PaneTarget,
        removed_pane_ids: Vec<rmux_core::PaneId>,
        pane_event: super::super::QueuedLifecycleEvent,
        session_event: super::super::QueuedLifecycleEvent,
        output: ExitedPaneOutput,
    },
}

struct ExitedPaneOutput {
    receiver: Option<PaneOutputReceiver>,
    sender: Option<PaneOutputSender>,
}

impl ExitedPaneOutput {
    fn capture(
        state: &HandlerState,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
    ) -> Self {
        let (receiver, sender) =
            state.runtime_pane_output_drain_handles(runtime_session_name, pane_id);
        Self { receiver, sender }
    }

    async fn ensure_eof(self, generation: Option<u64>, output_eof_published: bool) {
        if output_eof_published {
            return;
        }
        if wait_for_pane_output_eof(self.receiver).await {
            return;
        }
        if let Some(sender) = self.sender {
            let _ = sender.send_for_generation(generation, Vec::new());
        }
    }

    fn sender(&self) -> Option<PaneOutputSender> {
        self.sender.clone()
    }
}

impl RequestHandler {
    pub(in crate::handler) fn pane_exit_callback(&self) -> PaneExitCallback {
        let handler = self.downgrade();
        let runtime = self.server_task_runtime();
        std::sync::Arc::new(move |event: PaneExitEvent| {
            let Some(handler) = handler.upgrade() else {
                return;
            };
            let task = async move {
                handler.handle_pane_exit_event(event).await;
            };
            if let Some(runtime) = &runtime {
                runtime.spawn(task);
            } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(task);
            } else {
                tracing::warn!("dropping pane exit event because no Tokio runtime is available");
            }
        })
    }

    async fn handle_pane_exit_event(&self, event: PaneExitEvent) {
        let mut attempts = 0;
        let plan = loop {
            let plan = {
                let mut state = self.state.lock().await;
                let Some(runtime_session_name) = state.resolve_pane_event_runtime_session(
                    &event.session_name,
                    event.pane_id,
                    event.generation,
                ) else {
                    return;
                };
                let Some(target) =
                    state.pane_target_for_runtime_pane(&runtime_session_name, event.pane_id)
                else {
                    return;
                };
                let was_dead = state.pane_is_dead(target.session_name(), event.pane_id);
                let output =
                    ExitedPaneOutput::capture(&state, &runtime_session_name, event.pane_id);
                let metadata =
                    match state.observe_runtime_pane_exit(&runtime_session_name, event.pane_id) {
                        Ok(Some(metadata)) => Some(metadata),
                        Ok(None) => None,
                        Err(error) => {
                            warn!(
                                session = %runtime_session_name,
                                pane_id = event.pane_id.as_u32(),
                                "failed to observe pane exit: {error}"
                            );
                            return;
                        }
                    };

                if let Some(metadata) = metadata {
                    if should_keep_dead_pane(&state, &target, metadata) {
                        let (pane_id, window_id, window_name) =
                            pane_lifecycle_identifiers(&state, &target, event.pane_id);
                        let pane_event = prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::PaneDied {
                                target: target.clone(),
                                pane_id: Some(pane_id),
                                window_id,
                                window_name,
                            },
                        );
                        Some(PaneExitPlan::KeepDead {
                            runtime_session_name,
                            target,
                            prepare_dead: !was_dead,
                            pane_event,
                            output,
                        })
                    } else {
                        let Some(session) = state.sessions.session(target.session_name()) else {
                            return;
                        };
                        let Some(window) = session.window_at(target.window_index()) else {
                            return;
                        };
                        let only_window_remaining = session.windows().len() == 1;
                        let only_pane_remaining = window.pane_count() == 1;
                        let pane_id = window
                            .pane(target.pane_index())
                            .map(|pane| pane.id().as_u32())
                            .unwrap_or_else(|| event.pane_id.as_u32());
                        let window_id = window.id();
                        let window_name = window.name().unwrap_or_default().to_owned();
                        let _ = (session, window);
                        let pane_event = prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::PaneExited {
                                target: target.clone(),
                                pane_id: Some(pane_id),
                                window_id: Some(window_id.as_u32()),
                                window_name: Some(window_name.clone()),
                            },
                        );

                        if only_window_remaining && only_pane_remaining {
                            let current_runtime_owner =
                                state.sessions.runtime_owner(target.session_name());
                            let next_runtime_owner = state
                                .sessions
                                .runtime_owner_transfer_target(target.session_name());
                            let removed_session =
                                match state.sessions.remove_session(target.session_name()) {
                                    Ok(removed_session) => removed_session,
                                    Err(error) => {
                                        warn!(
                                            session = %target.session_name(),
                                            pane_id = event.pane_id.as_u32(),
                                            "failed to remove exited pane session: {error}"
                                        );
                                        return;
                                    }
                                };
                            let session_event = prepare_lifecycle_event(
                                &mut state,
                                &LifecycleEvent::SessionClosed {
                                    session_name: target.session_name().clone(),
                                    session_id: Some(removed_session.id().as_u32()),
                                },
                            );
                            let _ = state.options.remove_session(target.session_name());
                            let _ = state.environment.remove_session(target.session_name());
                            let _ = state.hooks.remove_session(target.session_name());
                            if let Err(error) = state.remove_session_terminals(
                                target.session_name(),
                                current_runtime_owner.as_ref(),
                                next_runtime_owner.as_ref(),
                            ) {
                                warn!(
                                    session = %target.session_name(),
                                    pane_id = event.pane_id.as_u32(),
                                    "failed to remove exited pane runtime state: {error}"
                                );
                            }
                            Some(PaneExitPlan::RemoveSession {
                                runtime_session_name,
                                session_name: target.session_name().clone(),
                                session_id: removed_session.id(),
                                target,
                                removed_pane_ids: vec![event.pane_id],
                                pane_event,
                                session_event,
                                output,
                            })
                        } else {
                            match state.kill_pane(target.clone()) {
                                Ok(result) => {
                                    if result.response.window_destroyed {
                                        let _ =
                                            state.hooks.remove_window(&WindowTarget::with_window(
                                                target.session_name().clone(),
                                                target.window_index(),
                                            ));
                                    } else {
                                        let _ = state.hooks.remove_pane(&target);
                                    }
                                    Some(PaneExitPlan::RemovePane {
                                        runtime_session_name,
                                        session_name: target.session_name().clone(),
                                        target,
                                        window_destroyed: result.response.window_destroyed,
                                        removed_pane_ids: result.removed_pane_ids,
                                        pane_event,
                                        output,
                                    })
                                }
                                Err(error) => {
                                    warn!(
                                        session = %target.session_name(),
                                        pane_id = event.pane_id.as_u32(),
                                        "failed to remove exited pane: {error}"
                                    );
                                    Some(PaneExitPlan::Ignore)
                                }
                            }
                        }
                    }
                } else {
                    None
                }
            };

            match plan {
                Some(plan) => break plan,
                None if attempts < PANE_EXIT_STATUS_RETRY_ATTEMPTS => {
                    attempts += 1;
                    tokio::time::sleep(PANE_EXIT_STATUS_RETRY_DELAY).await;
                }
                None => return,
            }
        };

        match plan {
            PaneExitPlan::Ignore => {}
            PaneExitPlan::KeepDead {
                runtime_session_name,
                target,
                prepare_dead,
                pane_event,
                output,
            } => {
                output
                    .ensure_eof(event.generation, event.output_eof_published())
                    .await;
                if prepare_dead {
                    self.prepare_kept_dead_pane_transcript(
                        &runtime_session_name,
                        event.pane_id,
                        &target,
                        event.output_eof_published(),
                    )
                    .await;
                }
                self.emit_prepared(pane_event);
                let session_names = if self.attached_count(target.session_name()).await == 0 {
                    let mut state = self.state.lock().await;
                    match apply_dead_pane_automatic_window_name(&mut state, &target) {
                        Ok(session_names) => session_names,
                        Err(error) => {
                            warn!(
                                session = %target.session_name(),
                                pane_index = target.pane_index(),
                                "failed to update dead pane automatic window name: {error}"
                            );
                            vec![target.session_name().clone()]
                        }
                    }
                } else {
                    vec![target.session_name().clone()]
                };
                for session_name in session_names {
                    self.refresh_attached_session(&session_name).await;
                    self.refresh_control_session(&session_name).await;
                }
            }
            PaneExitPlan::RemovePane {
                runtime_session_name,
                session_name,
                target,
                window_destroyed,
                removed_pane_ids,
                pane_event,
                output,
            } => {
                self.retain_removed_pane_output(
                    &runtime_session_name,
                    event.pane_id,
                    &target,
                    &output,
                );
                output
                    .ensure_eof(event.generation, event.output_eof_published())
                    .await;
                self.forget_pane_snapshot_coalescers(&removed_pane_ids);
                self.cleanup_exited_pane_output_subscription(&runtime_session_name, event.pane_id)
                    .await;
                self.emit_prepared(pane_event);
                self.sync_session_silence_timers(&session_name).await;
                if !window_destroyed {
                    self.emit(LifecycleEvent::WindowLayoutChanged {
                        target: WindowTarget::with_window(
                            session_name.clone(),
                            target.window_index(),
                        ),
                    })
                    .await;
                }
                self.refresh_attached_session(&session_name).await;
                self.refresh_control_session(&session_name).await;
            }
            PaneExitPlan::RemoveSession {
                runtime_session_name,
                session_name,
                session_id,
                target,
                removed_pane_ids,
                pane_event,
                session_event,
                output,
            } => {
                self.prune_web_session(Some((session_name.clone(), session_id)));
                self.retain_removed_pane_output(
                    &runtime_session_name,
                    event.pane_id,
                    &target,
                    &output,
                );
                self.remove_session_leases(std::slice::from_ref(&session_name));
                output
                    .ensure_eof(event.generation, event.output_eof_published())
                    .await;
                self.forget_pane_snapshot_coalescers(&removed_pane_ids);
                self.cleanup_exited_pane_output_subscription(&runtime_session_name, event.pane_id)
                    .await;
                self.exit_attached_session(&session_name).await;
                self.cancel_session_silence_timers(&session_name).await;
                self.emit_prepared(pane_event);
                self.emit_prepared(session_event);
                self.refresh_control_session(&session_name).await;
                let _ = self.request_shutdown_if_server_empty().await;
            }
        }
    }

    fn retain_removed_pane_output(
        &self,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
        target: &PaneTarget,
        output: &ExitedPaneOutput,
    ) {
        if let Some(sender) = output.sender() {
            self.retain_exited_pane_output(
                target.clone(),
                PaneOutputSubscriptionKey::new(runtime_session_name.clone(), pane_id),
                sender,
            );
        }
    }

    async fn cleanup_exited_pane_output_subscription(
        &self,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
    ) {
        let key = PaneOutputSubscriptionKey::new(runtime_session_name.clone(), pane_id);
        self.drain_exited_pane_output_subscriptions(key).await;
    }

    async fn prepare_kept_dead_pane_transcript(
        &self,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
        target: &PaneTarget,
        output_eof_published: bool,
    ) {
        let (retry_strip, output_rx) = {
            let mut state = self.state.lock().await;
            let output_rx = state.subscribe_runtime_pane_output(runtime_session_name, pane_id);
            let stripped = match state.strip_attached_submitted_line(runtime_session_name, pane_id)
            {
                Ok(stripped) => stripped,
                Err(error) => {
                    warn!(
                        session = %runtime_session_name,
                        pane_id = pane_id.as_u32(),
                        "failed to strip attached submitted line for dead pane: {error}"
                    );
                    false
                }
            };
            (!stripped, output_rx)
        };

        if retry_strip && !output_eof_published {
            // On Windows the child-exit watcher can beat the ConPTY reader.
            // Wait for the reader's EOF marker so a final echoed command can be
            // stripped before the dead-pane message is appended.
            wait_for_pane_output_eof(output_rx).await;
        }

        let mut state = self.state.lock().await;
        if retry_strip {
            if let Err(error) = state.strip_attached_submitted_line(runtime_session_name, pane_id) {
                warn!(
                    session = %runtime_session_name,
                    pane_id = pane_id.as_u32(),
                    "failed to retry attached submitted line strip for dead pane: {error}"
                );
            }
        }
        if let Err(error) = append_remain_on_exit_message(&mut state, runtime_session_name, target)
        {
            warn!(
                session = %runtime_session_name,
                pane_id = pane_id.as_u32(),
                "failed to append remain-on-exit message: {error}"
            );
        }
    }
}

async fn wait_for_pane_output_eof(output_rx: Option<PaneOutputReceiver>) -> bool {
    let Some(mut output_rx) = output_rx else {
        return false;
    };
    tokio::time::timeout(DEAD_PANE_OUTPUT_DRAIN_TIMEOUT, async move {
        loop {
            match output_rx.recv().await {
                OutputCursorItem::Event(event) if event.bytes().is_empty() => break,
                OutputCursorItem::Event(_) | OutputCursorItem::Gap(_) => {}
            }
        }
    })
    .await
    .is_ok()
}

fn should_keep_dead_pane(
    state: &HandlerState,
    target: &PaneTarget,
    metadata: PaneExitMetadata,
) -> bool {
    match state
        .options
        .resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::RemainOnExit,
        )
        .unwrap_or("off")
    {
        "on" | "key" => true,
        "failed" => metadata.signal.is_some() || metadata.status.is_some_and(|status| status != 0),
        _ => false,
    }
}

fn pane_lifecycle_identifiers(
    state: &HandlerState,
    target: &PaneTarget,
    fallback_pane_id: rmux_core::PaneId,
) -> (u32, Option<u32>, Option<String>) {
    let Some(session) = state.sessions.session(target.session_name()) else {
        return (fallback_pane_id.as_u32(), None, None);
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return (fallback_pane_id.as_u32(), None, None);
    };
    let pane_id = window
        .pane(target.pane_index())
        .map(|pane| pane.id().as_u32())
        .unwrap_or_else(|| fallback_pane_id.as_u32());
    (
        pane_id,
        Some(window.id().as_u32()),
        Some(window.name().unwrap_or_default().to_owned()),
    )
}

fn append_remain_on_exit_message(
    state: &mut HandlerState,
    runtime_session_name: &rmux_proto::SessionName,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    let template = state
        .options
        .resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::RemainOnExitFormat,
        )
        .unwrap_or_default();
    if template.is_empty() {
        return Ok(());
    }

    let runtime = format_context_for_target(state, &Target::Pane(target.clone()), 0)?;
    let rendered = render_runtime_template(template, &runtime, false);
    if rendered.is_empty() {
        return Ok(());
    }

    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            missing_pane_terminal(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
        })?;
    let rows = state
        .transcript_handle(target)?
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .clone_screen()
        .size()
        .rows
        .max(1);
    let mut bytes = format!("\x1b[{rows};1H\x1b[2K").into_bytes();
    bytes.extend_from_slice(rendered.as_bytes());
    state.append_bytes_to_runtime_pane_transcript(runtime_session_name, pane_id, &bytes)
}

fn apply_dead_pane_automatic_window_name(
    state: &mut HandlerState,
    target: &PaneTarget,
) -> Result<Vec<rmux_proto::SessionName>, RmuxError> {
    let rendered = state
        .pane_runtime_window_name_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.ends_with("[dead]") {
                value
            } else {
                format!("{value}[dead]")
            }
        })
        .unwrap_or_default();
    if rendered.is_empty() {
        return Ok(vec![target.session_name().clone()]);
    }

    let tracked = state.tracks_auto_named_window(target.session_name(), target.window_index());
    let should_update = {
        let session = state
            .sessions
            .session(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))?;
        let window = session.window_at(target.window_index()).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{}:{}", target.session_name(), target.window_index()),
                "window index does not exist in session",
            )
        })?;
        window.name() != Some(rendered.as_str())
            && crate::automatic_rename::window_allows_automatic_rename(
                &state.options,
                target.session_name(),
                target.window_index(),
                window,
                tracked,
            )
    };
    if !should_update {
        return Ok(vec![target.session_name().clone()]);
    }

    state
        .sessions
        .session_mut(target.session_name())
        .expect("existing session must accept automatic rename update")
        .window_at_mut(target.window_index())
        .expect("existing window must accept automatic rename update")
        .set_automatic_name(rendered);
    state.mark_auto_named_window(target.session_name(), target.window_index());
    state.synchronize_linked_window_from_slot(target.session_name(), target.window_index())?;
    Ok(state
        .synchronize_session_group_from(target.session_name())
        .unwrap_or_else(|_| vec![target.session_name().clone()]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_eof_skips_timeout_when_exit_event_already_published_eof() {
        let sender = crate::pane_io::pane_output_channel();
        let generation = None;
        sender
            .send_for_generation(generation, Vec::new())
            .expect("matching generation should accept EOF marker");

        let receiver = sender.subscribe();
        let output = ExitedPaneOutput {
            receiver: Some(receiver),
            sender: Some(sender),
        };

        tokio::time::timeout(
            Duration::from_millis(25),
            output.ensure_eof(generation, true),
        )
        .await
        .expect("published EOF should not wait for the drain timeout");
    }
}
