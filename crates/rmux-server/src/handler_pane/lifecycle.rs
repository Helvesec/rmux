use std::time::Duration;

use rmux_core::LifecycleEvent;
use rmux_proto::{OptionName, PaneTarget, RmuxError, Target, WindowTarget};

use super::super::{
    prepare_lifecycle_event, scripting_support::format_context_for_target, RequestHandler,
};
use crate::format_runtime::render_runtime_template;
use crate::pane_io::{PaneExitCallback, PaneExitEvent};
use crate::pane_terminal_lookup::missing_pane_terminal;
use crate::pane_terminals::{session_not_found, HandlerState, PaneExitMetadata};
use tokio::sync::broadcast;
use tracing::warn;

const PANE_EXIT_STATUS_RETRY_DELAY: Duration = Duration::from_millis(10);
const PANE_EXIT_STATUS_RETRY_ATTEMPTS: usize = 20;
const DEAD_PANE_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

enum PaneExitPlan {
    Ignore,
    KeepDead {
        target: PaneTarget,
        prepare_dead: bool,
    },
    RemovePane {
        session_name: rmux_proto::SessionName,
        target: PaneTarget,
        window_destroyed: bool,
        pane_event: super::super::QueuedLifecycleEvent,
    },
    RemoveSession {
        session_name: rmux_proto::SessionName,
        pane_event: super::super::QueuedLifecycleEvent,
        session_event: super::super::QueuedLifecycleEvent,
    },
}

impl RequestHandler {
    pub(in crate::handler) fn pane_exit_callback(&self) -> PaneExitCallback {
        let handler = self.downgrade();
        let runtime = tokio::runtime::Handle::current();
        std::sync::Arc::new(move |event: PaneExitEvent| {
            let Some(handler) = handler.upgrade() else {
                return;
            };
            runtime.spawn(async move {
                handler.handle_pane_exit_event(event).await;
            });
        })
    }

    async fn handle_pane_exit_event(&self, event: PaneExitEvent) {
        let mut attempts = 0;
        let plan = loop {
            let plan = {
                let mut state = self.state.lock().await;
                if !state.pane_output_generation_matches(
                    &event.session_name,
                    event.pane_id,
                    event.generation,
                ) {
                    return;
                }
                let Some(target) =
                    state.pane_target_for_runtime_pane(&event.session_name, event.pane_id)
                else {
                    return;
                };
                let was_dead = state.pane_is_dead(target.session_name(), event.pane_id);
                let metadata =
                    match state.observe_runtime_pane_exit(&event.session_name, event.pane_id) {
                        Ok(Some(metadata)) => Some(metadata),
                        Ok(None) => None,
                        Err(error) => {
                            warn!(
                                session = %event.session_name,
                                pane_id = event.pane_id.as_u32(),
                                "failed to observe pane exit: {error}"
                            );
                            return;
                        }
                    };

                if let Some(metadata) = metadata {
                    if should_keep_dead_pane(&state, &target, metadata) {
                        Some(PaneExitPlan::KeepDead {
                            target,
                            prepare_dead: !was_dead,
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
                                session_name: target.session_name().clone(),
                                pane_event,
                                session_event,
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
                                        session_name: target.session_name().clone(),
                                        target,
                                        window_destroyed: result.response.window_destroyed,
                                        pane_event,
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
                target,
                prepare_dead,
            } => {
                if prepare_dead {
                    self.prepare_kept_dead_pane_transcript(&event, &target)
                        .await;
                }
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
                session_name,
                target,
                window_destroyed,
                pane_event,
            } => {
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
                session_name,
                pane_event,
                session_event,
            } => {
                self.exit_attached_session(&session_name).await;
                self.cancel_session_silence_timers(&session_name).await;
                self.emit_prepared(pane_event);
                self.emit_prepared(session_event);
                self.refresh_control_session(&session_name).await;
                let _ = self.request_shutdown_if_server_empty().await;
            }
        }
    }

    async fn prepare_kept_dead_pane_transcript(&self, event: &PaneExitEvent, target: &PaneTarget) {
        let (retry_strip, output_rx) = {
            let mut state = self.state.lock().await;
            let cleared_respawned_transcript = match state
                .clear_runtime_pane_transcript_for_dead_exit_if_marked(
                    &event.session_name,
                    event.pane_id,
                ) {
                Ok(cleared) => cleared,
                Err(error) => {
                    warn!(
                        session = %event.session_name,
                        pane_id = event.pane_id.as_u32(),
                        "failed to prepare dead pane transcript: {error}"
                    );
                    false
                }
            };
            let output_rx = (!cleared_respawned_transcript)
                .then(|| state.subscribe_runtime_pane_output(&event.session_name, event.pane_id))
                .flatten();
            let stripped = if cleared_respawned_transcript {
                false
            } else {
                match state.strip_attached_submitted_line(&event.session_name, event.pane_id) {
                    Ok(stripped) => stripped,
                    Err(error) => {
                        warn!(
                            session = %event.session_name,
                            pane_id = event.pane_id.as_u32(),
                            "failed to strip attached submitted line for dead pane: {error}"
                        );
                        false
                    }
                }
            };
            (!cleared_respawned_transcript && !stripped, output_rx)
        };

        if retry_strip {
            // On Windows the child-exit watcher can beat the ConPTY reader.
            // Wait for the reader's EOF marker so a final echoed command can be
            // stripped before the dead-pane message is appended.
            wait_for_pane_output_eof(output_rx).await;
        }

        let mut state = self.state.lock().await;
        if retry_strip {
            if let Err(error) =
                state.strip_attached_submitted_line(&event.session_name, event.pane_id)
            {
                warn!(
                    session = %event.session_name,
                    pane_id = event.pane_id.as_u32(),
                    "failed to retry attached submitted line strip for dead pane: {error}"
                );
            }
        }
        if let Err(error) = append_remain_on_exit_message(&mut state, &event.session_name, target) {
            warn!(
                session = %event.session_name,
                pane_id = event.pane_id.as_u32(),
                "failed to append remain-on-exit message: {error}"
            );
        }
    }
}

async fn wait_for_pane_output_eof(output_rx: Option<broadcast::Receiver<Vec<u8>>>) {
    let Some(mut output_rx) = output_rx else {
        return;
    };
    let _ = tokio::time::timeout(DEAD_PANE_OUTPUT_DRAIN_TIMEOUT, async move {
        loop {
            match output_rx.recv().await {
                Ok(bytes) if bytes.is_empty() => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
    .await;
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
            && (tracked || window.automatic_rename() || window.name().is_none())
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
