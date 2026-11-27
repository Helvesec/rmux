use std::io;

use super::super::{
    focus_support::prepare_server_focus_effects, prepare_lifecycle_event, RequestHandler,
};
use super::io_other;
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminals::session_not_found;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    types::OptionScopeSelector, ErrorResponse, HookName, OptionName, PaneTarget, Response,
    RmuxError, ScopeSelector, SelectPaneResponse, SessionName, SetOptionMode, Target, WindowTarget,
};

impl RequestHandler {
    pub(in crate::handler) async fn select_attached_mouse_focus(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        expected_window_id: u32,
        pane_id: rmux_core::PaneId,
    ) -> io::Result<()> {
        let selected_window = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach
                .by_pid
                .get(&identity.attach_pid())
                .filter(|active| {
                    identity.matches_active_session(active, session_name, session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                        && active.can_write
                        && !active
                            .flags
                            .contains(crate::client_flags::ClientFlags::READONLY)
                })
            else {
                return Ok(());
            };
            let _ = active;
            super::super::require_expected_session_identity(&state, session_name)
                .map_err(io_other)?;
            (|| -> io::Result<
                Option<(
                    Vec<SessionName>,
                    super::super::focus_support::FocusEffects,
                    super::super::QueuedLifecycleEvent,
                )>,
            > {
                let (window_index, pane_index) = {
                    let session = state
                        .sessions
                        .session(session_name)
                        .ok_or_else(|| io_other("attached session disappeared"))?;
                    let window_index = session.active_window_index();
                    let window = session.window();
                    if window.id().as_u32() != expected_window_id {
                        return Ok(None);
                    }
                    let Some(pane_index) = window
                        .panes()
                        .iter()
                        .find(|pane| pane.id() == pane_id)
                        .map(|pane| pane.index())
                    else {
                        return Ok(None);
                    };
                    if window.active_pane_index() == pane_index {
                        return Ok(None);
                    }
                    (window_index, pane_index)
                };
                let window_target = WindowTarget::with_window(session_name.clone(), window_index);
                let (_, refresh_sessions) = state
                    .mutate_window_and_resize_terminal_with_family(&window_target, |selection| {
                        selection.select(pane_index, false).map(|_| ())
                    })
                    .map_err(io_other)?;
                let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
                let selection_event = prepare_lifecycle_event(
                    &mut state,
                    &LifecycleEvent::WindowPaneChanged {
                        target: WindowTarget::with_window(session_name.clone(), window_index),
                    },
                );
                Ok(Some((
                    refresh_sessions,
                    focus_effects,
                    selection_event,
                )))
            })()?
        };

        let Some((refresh_sessions, focus_effects, selection_event)) = selected_window else {
            return Ok(());
        };
        focus_effects.execute(self).await?;
        self.emit_prepared(selection_event).await;
        self.refresh_attached_session_for_session_identity(session_name, session_id)
            .await;
        self.refresh_linked_window_sessions(
            refresh_sessions
                .into_iter()
                .filter(|candidate| candidate != session_name),
        )
        .await;
        Ok(())
    }

    pub(in crate::handler) async fn refresh_linked_window_sessions(
        &self,
        sessions: impl IntoIterator<Item = SessionName>,
    ) {
        for session_name in sessions {
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
        }
    }

    pub(in crate::handler) async fn handle_last_pane(
        &self,
        request: rmux_proto::LastPaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let selects_pane = request.input_disabled.is_none();
        let (response, refresh_sessions, focus_effects, selection_event) = {
            let mut state = self.state.lock().await;
            match state.last_pane(
                request.target,
                request.preserve_zoom,
                request.input_disabled,
            ) {
                Ok((response, refresh_sessions)) if !selects_pane => {
                    (Response::LastPane(response), refresh_sessions, None, None)
                }
                Ok((response, refresh_sessions)) => {
                    let mut active_attach = self.active_attach.lock().await;
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let event = prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::WindowPaneChanged {
                            target: WindowTarget::with_window(
                                session_name.clone(),
                                response.target.window_index(),
                            ),
                        },
                    );
                    (
                        Response::LastPane(response),
                        refresh_sessions,
                        Some(focus_effects),
                        Some(event),
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if matches!(response, Response::LastPane(_)) {
            if let Some(focus_effects) = focus_effects {
                let _ = focus_effects.execute(self).await;
            }
            if let Some(event) = selection_event {
                self.emit_prepared(event).await;
            }
            // See handle_select_pane below: skip refresh on a session with no
            // attached/control client so an unrelated deferred pane wait cannot
            // stall a detached select.
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }

        response
    }

    pub(in crate::handler) async fn handle_select_pane(
        &self,
        request: rmux_proto::SelectPaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let pane_index = request.target.pane_index();
        let title = request.title.clone();
        let style = request.style.clone();
        let input_disabled = request.input_disabled;
        let (
            response,
            pane_changed,
            title_changed_target,
            refresh_sessions,
            focus_effects,
            selection_event,
        ) = {
            let mut state = self.state.lock().await;
            let pane_changed = title.is_none()
                && input_disabled.is_none()
                && state
                    .sessions
                    .resolve_window(&Target::Window(WindowTarget::with_window(
                        session_name.clone(),
                        window_index,
                    )))
                    .is_ok_and(|window| window.active_pane_index() != pane_index);
            let mut title_changed_target = None;
            let mut title_state_event = None;
            let mut pane_option_event = None;
            let mut refresh_sessions = Vec::new();
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                super::super::require_expected_pane_identity(&state, &request.target)?;
                if let Some(style) = style.as_deref() {
                    let scope = OptionScopeSelector::Pane(request.target.clone());
                    rmux_core::validate_option_name_mutation(
                        "window-style",
                        &scope,
                        SetOptionMode::Replace,
                        Some(style),
                        false,
                    )?;
                }
                let response_target = if let Some(title) = title.as_deref() {
                    if let Some((old, new)) = state.set_pane_title(&request.target, title)? {
                        title_changed_target = Some(request.target.clone());
                        if let Some(pane_id) = pane_id_for_select_target(&state, &request.target) {
                            let generation =
                                state.pane_output_generation_for_target(&request.target, pane_id);
                            title_state_event = Some((pane_id, generation, old, new));
                        }
                    }
                    request.target.clone()
                } else if let Some(disabled) = input_disabled {
                    state.set_pane_input_disabled(&request.target, disabled)?;
                    request.target.clone()
                } else {
                    let window_target =
                        WindowTarget::with_window(session_name.clone(), window_index);
                    let select_target = |selection: rmux_core::WindowSelection<'_>| {
                        selection.select(pane_index, request.preserve_zoom)
                    };
                    let (active_pane_index, synchronized_sessions) = if pane_changed {
                        state
                            .mutate_window_naming_pane_and_resize_terminal_with_family_if(
                                &request.target,
                                select_target,
                                |_| true,
                            )
                            .map_err(RmuxError::from)
                    } else {
                        // Re-selecting the active pane only updates its activity.
                        // Its active identity, zoom state, and geometry stay unchanged,
                        // so synchronize aliases without requiring a live terminal.
                        state.mutate_window_and_synchronize_family(&window_target, select_target)
                    }?;
                    refresh_sessions = synchronized_sessions;
                    PaneTarget::with_window(session_name.clone(), window_index, active_pane_index)
                };
                if let Some(style) = style {
                    let outcome = state.options.set_by_name(
                        OptionScopeSelector::Pane(request.target.clone()),
                        "window-style",
                        Some(style),
                        SetOptionMode::Replace,
                        false,
                        false,
                        false,
                    )?;
                    state.synchronize_pane_alias_options_from_target(&request.target)?;
                    if outcome.changed {
                        if let Some(pane_id) = pane_id_for_select_target(&state, &request.target) {
                            let generation =
                                state.pane_output_generation_for_target(&request.target, pane_id);
                            pane_option_event = Some((pane_id, generation, outcome));
                        }
                    }
                    state.refresh_transcript_limits_for_scope(
                        &ScopeSelector::Pane(request.target.clone()),
                        OptionName::WindowStyle,
                    );
                }

                Ok(SelectPaneResponse {
                    target: response_target,
                })
            })() {
                Ok(response) => {
                    if let Some((pane_id, generation, old, new)) = &title_state_event {
                        self.record_pane_state_change(
                            *pane_id,
                            Some(*generation),
                            PaneStateChange::TitleChanged {
                                old: old.clone(),
                                new: new.clone(),
                            },
                        );
                    }
                    if let Some((pane_id, generation, outcome)) = pane_option_event.as_ref() {
                        self.record_pane_option_mutation(*pane_id, Some(*generation), outcome);
                    }
                    let (focus_effects, selection_event) = if pane_changed {
                        let mut active_attach = self.active_attach.lock().await;
                        let focus_effects =
                            prepare_server_focus_effects(&mut state, &mut active_attach);
                        let event = prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowPaneChanged {
                                target: WindowTarget::with_window(
                                    session_name.clone(),
                                    window_index,
                                ),
                            },
                        );
                        (Some(focus_effects), Some(event))
                    } else {
                        (None, None)
                    };
                    (
                        Response::SelectPane(response),
                        pane_changed,
                        title_changed_target,
                        refresh_sessions,
                        focus_effects,
                        selection_event,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    false,
                    None,
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            if pane_changed {
                if let Some(focus_effects) = focus_effects {
                    let _ = focus_effects.execute(self).await;
                }
                self.emit_prepared(selection_event.expect("changed pane prepares event"))
                    .await;
            }
            if let Some(target) = title_changed_target {
                self.emit(LifecycleEvent::PaneTitleChanged { target }).await;
            }
            if pane_changed {
                let Response::SelectPane(success) = &response else {
                    unreachable!("successful select-pane response was checked above");
                };
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            } else {
                self.queue_suppressed_inline_hook(
                    HookName::AfterSelectPane,
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            // A select-pane on a session with no attached or control client has
            // nothing to draw, so skip the refresh — and, on Windows, its
            // deferred-pane wait — instead of stalling ~2s on the session's own
            // just-spawned pane while it is still starting
            // (unrelated_starting_pane_does_not_block_a_stable_session_refresh).
            if refresh_sessions.is_empty() {
                if self.attached_count(&session_name).await > 0 {
                    self.refresh_attached_session(&session_name).await;
                }
            } else {
                self.refresh_linked_window_sessions(refresh_sessions).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_select_pane_adjacent(
        &self,
        request: rmux_proto::SelectPaneAdjacentRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let anchor_pane_index = request.target.pane_index();
        let (response, pane_changed, refresh_sessions, focus_effects, selection_event) = {
            let mut state = self.state.lock().await;
            let active_before = state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(window_index))
                .map(|window| window.active_pane_index());
            match (|| -> Result<(SelectPaneResponse, bool, Vec<SessionName>), RmuxError> {
                let (active_pane_index, refresh_sessions) = state
                    .mutate_window_naming_pane_and_resize_terminal_with_family_if(
                        &request.target,
                        |selection| {
                            selection.select_adjacent(
                                anchor_pane_index,
                                request.direction,
                                request.preserve_zoom,
                            )
                        },
                        |active_pane_index| {
                            active_before.is_some_and(|before| before != *active_pane_index)
                        },
                    )?;
                let pane_changed = active_before.is_some_and(|before| before != active_pane_index);
                Ok((
                    SelectPaneResponse {
                        target: PaneTarget::with_window(
                            session_name.clone(),
                            window_index,
                            active_pane_index,
                        ),
                    },
                    pane_changed,
                    refresh_sessions,
                ))
            })() {
                Ok((response, pane_changed, refresh_sessions)) => {
                    let (focus_effects, selection_event) = if pane_changed {
                        let mut active_attach = self.active_attach.lock().await;
                        let focus_effects =
                            prepare_server_focus_effects(&mut state, &mut active_attach);
                        let event = prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::WindowPaneChanged {
                                target: WindowTarget::with_window(
                                    session_name.clone(),
                                    window_index,
                                ),
                            },
                        );
                        (Some(focus_effects), Some(event))
                    } else {
                        (None, None)
                    };
                    (
                        Response::SelectPane(response),
                        pane_changed,
                        refresh_sessions,
                        focus_effects,
                        selection_event,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    false,
                    Vec::new(),
                    None,
                    None,
                ),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            if pane_changed {
                if let Some(focus_effects) = focus_effects {
                    let _ = focus_effects.execute(self).await;
                }
                self.emit_prepared(selection_event.expect("changed pane prepares event"))
                    .await;
                let Response::SelectPane(success) = &response else {
                    unreachable!("successful adjacent select-pane response was checked above");
                };
                self.queue_inline_hook(
                    HookName::AfterSelectPane,
                    ScopeSelector::Session(session_name.clone()),
                    Some(Target::Pane(success.target.clone())),
                    PendingInlineHookFormat::AfterCommand,
                );
            } else {
                self.queue_suppressed_inline_hook(
                    HookName::AfterSelectPane,
                    PendingInlineHookFormat::AfterCommand,
                );
            }
            if refresh_sessions.is_empty() {
                if self.attached_count(&session_name).await > 0 {
                    self.refresh_attached_session(&session_name).await;
                }
            } else {
                self.refresh_linked_window_sessions(refresh_sessions).await;
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_select_pane_mark(
        &self,
        request: rmux_proto::SelectPaneMarkRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let (response, title_changed_target) = {
            let mut state = self.state.lock().await;
            let mut title_changed_target = None;
            let mut title_state_event = None;
            match (|| -> Result<SelectPaneResponse, RmuxError> {
                if let Some(title) = request.title.as_deref() {
                    if let Some((old, new)) = state.set_pane_title(&request.target, title)? {
                        title_changed_target = Some(request.target.clone());
                        if let Some(pane_id) = pane_id_for_select_target(&state, &request.target) {
                            let generation =
                                state.pane_output_generation_for_target(&request.target, pane_id);
                            title_state_event = Some((pane_id, generation, old, new));
                        }
                    }
                }
                if request.clear {
                    state.clear_marked_pane();
                } else {
                    let _ = state.toggle_marked_pane(&request.target)?;
                }

                let session = state
                    .sessions
                    .session(&session_name)
                    .ok_or_else(|| session_not_found(&session_name))?;
                let active_pane_index = session
                    .window_at(window_index)
                    .ok_or_else(|| {
                        RmuxError::invalid_target(
                            format!("{session_name}:{window_index}"),
                            "window index does not exist in session",
                        )
                    })?
                    .active_pane_index();
                Ok(SelectPaneResponse {
                    target: PaneTarget::with_window(
                        session_name.clone(),
                        window_index,
                        active_pane_index,
                    ),
                })
            })() {
                Ok(response) => {
                    if let Some((pane_id, generation, old, new)) = &title_state_event {
                        self.record_pane_state_change(
                            *pane_id,
                            Some(*generation),
                            PaneStateChange::TitleChanged {
                                old: old.clone(),
                                new: new.clone(),
                            },
                        );
                    }
                    (Response::SelectPane(response), title_changed_target)
                }
                Err(error) => (Response::Error(ErrorResponse { error }), None),
            }
        };

        if matches!(response, Response::SelectPane(_)) {
            if let Some(target) = title_changed_target {
                self.emit(LifecycleEvent::PaneTitleChanged { target }).await;
            }
            if self.attached_count(&session_name).await > 0 {
                self.refresh_attached_session(&session_name).await;
            }
            self.refresh_control_session(&session_name).await;
        }

        response
    }
}

fn pane_id_for_select_target(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
) -> Option<rmux_core::PaneId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
}

#[cfg(test)]
#[path = "../../handler_pane/selection_tests.rs"]
mod tests;
