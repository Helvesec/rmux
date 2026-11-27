use rmux_core::LifecycleEvent;
use rmux_proto::{PaneTarget, RmuxError, SessionName, WindowTarget};

use super::super::RequestHandler;
use super::mode_tree_model::{ModeTreeActionIdentity, ModeTreeClientState};
use crate::handler::attach_support::{ActiveAttach, ActiveAttachState};
use crate::handler::copy_mode_support::cancel_all::{PaneModePopReceipt, PaneModeTransition};
use crate::handler_support::attached_client_required;
use crate::pane_io::AttachControl;
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::SharedPaneTranscript;

pub(in crate::handler) struct ModeTreeDismissPlan {
    attach_pid: u32,
    attach_id: u64,
    mode_tree_state_id: u64,
    mode: ModeTreeClientState,
}

pub(in crate::handler) struct ModeTreeDismissEffects {
    pane_mode_changed: Option<PaneTarget>,
    refresh_sessions: Vec<SessionName>,
    cleanup_errors: Vec<String>,
}

/// One client's own tree state, with the frame, the identifier every persistent
/// overlay is stamped with and the epoch the renderer compares against.
fn clear_one_active_mode_tree(active: &mut ActiveAttach) {
    if active.mode_tree.take().is_none() {
        return;
    }
    active.mode_tree_frame = None;
    active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
    active.persistent_overlay_epoch.store(
        active.mode_tree_state_id,
        std::sync::atomic::Ordering::SeqCst,
    );
    active.overlay_generation = active.overlay_generation.saturating_add(1);
    let _ = active
        .control_tx
        .send(AttachControl::AdvancePersistentOverlayState(
            active.mode_tree_state_id,
        ));
}

fn clear_active_mode_trees_for_session(
    active_attach: &mut ActiveAttachState,
    session_name: &SessionName,
    session_id: rmux_proto::SessionId,
) {
    for active in active_attach.by_pid.values_mut() {
        if &active.session_name != session_name || active.session_id != session_id {
            continue;
        }
        clear_one_active_mode_tree(active);
    }
}

/// The host a tree recorded, when it still answers for it: the same pane, at the
/// same output generation, on the same transcript instance.
///
/// These are the checks `cleanup_mode_tree_before_removal` makes before it
/// notifies, and this is the target it notifies. A `-q` reached through another
/// alias keeps the same provenance rather than renaming it after the caller.
fn valid_mode_tree_host(state: &HandlerState, mode: &ModeTreeClientState) -> Option<PaneTarget> {
    let identity = mode.host_identity.as_ref()?;
    let host = identity.current_target(state)?;
    let retained = mode.host_transcript.as_ref()?;
    (identity.output_generation_matches(state, &host)
        && state
            .transcript_handle(&host)
            .is_ok_and(|current| current.same_instance(retained)))
    .then_some(host)
}

/// The zoom a tree took, given back, with the sessions its layout touched.
///
/// This is the block `cleanup_mode_tree_before_removal` runs after its own
/// clear; `-q` reuses it without that clear, having popped under its own guard.
fn restore_mode_tree_zoom(
    state: &mut HandlerState,
    mode: &ModeTreeClientState,
    refresh: &mut Vec<SessionName>,
) -> Result<(), RmuxError> {
    let Some(identity) = mode.zoom_restore.as_ref() else {
        return Ok(());
    };
    let Some(target) = identity.current_target(state) else {
        return Ok(());
    };
    let needs_restore = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .is_some_and(rmux_core::Window::is_zoomed);
    if !needs_restore {
        return Ok(());
    }
    let window_target =
        WindowTarget::with_window(target.session_name().clone(), target.window_index());
    let ((), refresh_sessions) = state.mutate_window_layout_and_resize_terminal_with_family(
        &window_target,
        |mut window_layout| {
            if window_layout.is_zoomed() {
                window_layout.toggle_zoom(target.pane_index())?;
            }
            Ok(())
        },
    )?;
    for session_name in refresh_sessions {
        push_refresh_session(refresh, session_name);
    }
    Ok(())
}

/// Puts back what a `-q` pop committed, when the cleanup that followed it could
/// not finish.
///
/// It restores only where that pop is still the last word: the same transcript
/// instance, and the revision that pop left. A consultation that entered since
/// has a revision of its own and is not overwritten. The guard is released
/// before the restoration's own resize, which re-takes the transcript.
fn restore_mode_tree_after_committed_pop(
    state: &mut HandlerState,
    target: &PaneTarget,
    transcript: &SharedPaneTranscript,
    receipt: &PaneModePopReceipt,
) {
    if !state
        .transcript_handle(target)
        .is_ok_and(|current| current.same_instance(transcript))
    {
        return;
    }
    {
        let mut guard = transcript.lock();
        if guard.pane_mode_revision() != receipt.revision() {
            return;
        }
        guard.enter_mode_tree(receipt.popped_name());
    }
    let _ = state.resize_terminals(target.session_name());
}

fn push_refresh_session(refresh: &mut Vec<SessionName>, session_name: SessionName) {
    if !refresh.iter().any(|candidate| candidate == &session_name) {
        refresh.push(session_name);
    }
}

fn restore_transcript_mode_after_failed_cleanup(
    state: &mut HandlerState,
    mode: &ModeTreeClientState,
    transcript: &SharedPaneTranscript,
    target: Option<&PaneTarget>,
) {
    transcript
        .lock()
        .enter_mode_tree(mode.kind.pane_mode_name());
    if let Some(target) = target {
        let _ = state.resize_terminals(target.session_name());
    }
}

fn cleanup_mode_tree_before_removal(
    state: &mut HandlerState,
    mode: &ModeTreeClientState,
    preserve_mode_on_failure: bool,
) -> Result<ModeTreeDismissEffects, RmuxError> {
    let mut effects = ModeTreeDismissEffects {
        pane_mode_changed: None,
        refresh_sessions: vec![mode.session_name.clone()],
        cleanup_errors: Vec::new(),
    };
    let current_host = mode
        .host_identity
        .as_ref()
        .and_then(|identity| identity.current_target(state));
    let host_transcript_is_current = mode
        .host_identity
        .as_ref()
        .zip(current_host.as_ref())
        .zip(mode.host_transcript.as_ref())
        .is_some_and(|((identity, target), retained)| {
            identity.output_generation_matches(state, target)
                && state
                    .transcript_handle(target)
                    .is_ok_and(|current| current.same_instance(retained))
        });
    let transcript_changed = mode
        .host_transcript
        .as_ref()
        .is_some_and(|transcript| transcript.lock().clear_mode_tree());
    if transcript_changed && host_transcript_is_current {
        let target = current_host
            .as_ref()
            .expect("a current transcript requires a current host target");
        if let Err(error) = state.resize_terminals(target.session_name()) {
            if preserve_mode_on_failure {
                restore_transcript_mode_after_failed_cleanup(
                    state,
                    mode,
                    mode.host_transcript
                        .as_ref()
                        .expect("changed transcript exists"),
                    Some(target),
                );
                return Err(error);
            }
            effects.cleanup_errors.push(format!(
                "failed to resize restored pane mode after committed switch: {error}"
            ));
        }
        effects.pane_mode_changed = Some(target.clone());
    }

    if let Some(identity) = mode.zoom_restore.as_ref() {
        if let Some(target) = identity.current_target(state) {
            let needs_restore = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .is_some_and(rmux_core::Window::is_zoomed);
            if needs_restore {
                let window_target =
                    WindowTarget::with_window(target.session_name().clone(), target.window_index());
                let restore = state.mutate_window_layout_and_resize_terminal_with_family(
                    &window_target,
                    |mut window_layout| {
                        if window_layout.is_zoomed() {
                            window_layout.toggle_zoom(target.pane_index())?;
                        }
                        Ok(())
                    },
                );
                match restore {
                    Ok(((), refresh_sessions)) => {
                        for session_name in refresh_sessions {
                            push_refresh_session(&mut effects.refresh_sessions, session_name);
                        }
                    }
                    Err(error) => {
                        if preserve_mode_on_failure {
                            if transcript_changed {
                                if let Some(transcript) = mode.host_transcript.as_ref() {
                                    restore_transcript_mode_after_failed_cleanup(
                                        state,
                                        mode,
                                        transcript,
                                        host_transcript_is_current.then_some(&target),
                                    );
                                }
                            }
                            return Err(error);
                        }
                        effects.cleanup_errors.push(format!(
                            "failed to restore mode-tree zoom after committed switch: {error}"
                        ));
                    }
                }
            }
        }
    }
    Ok(effects)
}

impl RequestHandler {
    pub(in crate::handler) fn prepare_mode_tree_dismissal_for_committed_switch(
        &self,
        _state: &HandlerState,
        active_attach: &ActiveAttachState,
        attach_pid: u32,
        expected_attach_id: u64,
    ) -> Result<Option<ModeTreeDismissPlan>, RmuxError> {
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .filter(|active| {
                active.id == expected_attach_id
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| attached_client_required("switch-client"))?;
        let Some(mode) = active.mode_tree.clone() else {
            return Ok(None);
        };
        Ok(Some(ModeTreeDismissPlan {
            attach_pid,
            attach_id: expected_attach_id,
            mode_tree_state_id: active.mode_tree_state_id,
            mode,
        }))
    }

    pub(in crate::handler) fn apply_committed_mode_tree_dismissal(
        &self,
        state: &mut HandlerState,
        active_attach: &mut ActiveAttachState,
        plan: ModeTreeDismissPlan,
        committed_session_name: &SessionName,
    ) -> ModeTreeDismissEffects {
        let mut effects = ModeTreeDismissEffects {
            pane_mode_changed: None,
            refresh_sessions: Vec::new(),
            cleanup_errors: Vec::new(),
        };
        let source_is_current = active_attach
            .by_pid
            .get(&plan.attach_pid)
            .is_some_and(|active| {
                active.id == plan.attach_id
                    && active.mode_tree_state_id == plan.mode_tree_state_id
                    && active.mode_tree.is_some()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            });
        if !source_is_current {
            effects
                .cleanup_errors
                .push("mode-tree identity changed after committed switch delivery".to_owned());
            return effects;
        }

        match cleanup_mode_tree_before_removal(state, &plan.mode, false) {
            Ok(cleanup) => effects = cleanup,
            Err(error) => effects.cleanup_errors.push(format!(
                "failed to clean mode-tree before committed dismissal: {error}"
            )),
        }
        if committed_session_name != &plan.mode.session_name {
            // The switch frame was necessarily queued before the dismissal
            // committed. A grouped/linked target can share the zoomed source
            // window, so refresh the newly attached identity after cleanup to
            // guarantee its next queued frame reflects the restored layout.
            push_refresh_session(
                &mut effects.refresh_sessions,
                committed_session_name.clone(),
            );
        }
        clear_active_mode_trees_for_session(
            active_attach,
            &plan.mode.session_name,
            plan.mode.session_id,
        );
        effects
    }

    pub(in crate::handler) async fn finish_committed_mode_tree_dismissal(
        &self,
        effects: ModeTreeDismissEffects,
    ) {
        if let Some(target) = effects.pane_mode_changed {
            self.sync_automatic_window_name_for_pane_target(&target)
                .await;
            self.emit_without_attached_refresh(LifecycleEvent::PaneModeChanged { target })
                .await;
        }
        for session_name in effects.refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        for error in effects.cleanup_errors {
            tracing::warn!(error = %error, "committed switch mode-tree cleanup was incomplete");
        }
    }

    pub(super) async fn dismiss_mode_tree_with_refresh_for_action_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<(), RmuxError> {
        let attach_pid = identity.attach_pid();
        let session_name = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == identity.attach_id()
                        && active.mode_tree_state_id == identity.state_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|active| active.session_name.clone())
                .ok_or_else(|| attached_client_required("choose-buffer"))?
        };
        let session_names = self.dismiss_mode_tree_for_action_identity(identity).await?;
        let _ = self
            .refresh_attached_client_for_identity(
                attach_pid,
                identity.attach_id(),
                &session_name,
                "choose-buffer",
            )
            .await;
        tokio::task::yield_now().await;
        for session_name in session_names {
            self.refresh_attached_session(&session_name).await;
        }
        Ok(())
    }

    pub(in crate::handler) async fn dismiss_mode_tree(
        &self,
        attach_pid: u32,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.dismiss_mode_tree_with_expected_identity(attach_pid, None, None)
            .await
    }

    pub(in crate::handler) async fn dismiss_mode_tree_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.dismiss_mode_tree_with_expected_identity(attach_pid, Some(expected_attach_id), None)
            .await
    }

    pub(super) async fn dismiss_mode_tree_for_action_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.dismiss_mode_tree_with_expected_identity(
            identity.attach_pid(),
            Some(identity.attach_id()),
            Some(identity.state_id()),
        )
        .await
    }

    /// Dismisses every tree hosted by this pane, whoever is showing it.
    ///
    /// `copy-mode -q` addresses a pane, not a client: what goes are the trees
    /// whose HOST is that pane - by the transcript they are really on and by the
    /// pane identity and output generation they kept for it - so a client or an
    /// alias sharing that host is covered while a tree hosted elsewhere in the
    /// same session is not. The per-session clearing above stays for the client
    /// paths that own it.
    pub(in crate::handler) async fn dismiss_mode_trees_hosted_by_pane(
        &self,
        transition: &PaneModeTransition,
    ) -> Result<Option<PaneModePopReceipt>, RmuxError> {
        let target = transition.target().clone();
        let target = &target;
        let (effects, receipt) = {
            let mut state = self.state.lock().await;
            // The pane and its transcript instance live here, so they answer
            // here; the revision and the pop belong to the guard below.
            if !transition.matches_identity(&state) {
                return Ok(None);
            }
            let transcript = transition.transcript().clone();
            #[cfg(test)]
            self.pause_before_cancel_all_tree_attach_for_test(target)
                .await;
            // The clients are taken BEFORE the atomic check, so nothing this
            // command waits for can fall between that check and its pop.
            let mut active_attach = self.active_attach.lock().await;
            let Some(receipt) = transition.pop(&mut transcript.lock()) else {
                return Ok(None);
            };
            let hosts = active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    active
                        .mode_tree
                        .as_ref()
                        .is_some_and(|mode| mode.hosts_pane(&state, target, &transcript))
                })
                .map(|(attach_pid, _)| *attach_pid)
                .collect::<Vec<_>>();
            // What these effects are about is the host the tree recorded and
            // still answers for - the pane it was opened on, at its own session -
            // not the alias this command happened to be addressed by. A hook
            // reads that provenance, so it is the one the resize, the event and
            // the refresh keep. The requested target remains what answers when no
            // client hosts the tree at all, as it did before.
            let notified_host = hosts
                .iter()
                .find_map(|attach_pid| {
                    active_attach
                        .by_pid
                        .get(attach_pid)
                        .and_then(|active| active.mode_tree.as_ref())
                        .and_then(|mode| valid_mode_tree_host(&state, mode))
                })
                .unwrap_or_else(|| target.clone());
            // The guard is gone: what follows re-takes the transcript of its own
            // accord, so none of it may run while this command holds it.
            let mut effects = ModeTreeDismissEffects {
                pane_mode_changed: Some(notified_host.clone()),
                refresh_sessions: vec![notified_host.session_name().clone()],
                cleanup_errors: Vec::new(),
            };
            // The pop said what it uncovered, in the guard that popped: only a
            // consultation underneath can give a modal reserve back, and only
            // then can this dismissal change a size. Asking the endpoint for a
            // size it already holds is not free on Windows - the console
            // answers an identical resize with a full repaint, and those bytes
            // reach the panes of this session as if their own tools had written
            // them while their modes were up.
            if receipt.uncovered_copy_mode()
                && state.copy_mode_transition_changes_terminal_geometry(&notified_host)
            {
                if let Err(error) = state.resize_terminals(notified_host.session_name()) {
                    restore_mode_tree_after_committed_pop(
                        &mut state,
                        target,
                        &transcript,
                        &receipt,
                    );
                    return Err(error);
                }
            }
            for attach_pid in &hosts {
                let Some(mode) = active_attach
                    .by_pid
                    .get(attach_pid)
                    .and_then(|active| active.mode_tree.clone())
                else {
                    continue;
                };
                push_refresh_session(&mut effects.refresh_sessions, mode.session_name.clone());
                if let Err(error) =
                    restore_mode_tree_zoom(&mut state, &mode, &mut effects.refresh_sessions)
                {
                    restore_mode_tree_after_committed_pop(
                        &mut state,
                        target,
                        &transcript,
                        &receipt,
                    );
                    return Err(error);
                }
            }
            // The clients of that host lose their tree here: no second pop and
            // no second receipt, only the state they were holding for it.
            for attach_pid in &hosts {
                if let Some(active) = active_attach.by_pid.get_mut(attach_pid) {
                    clear_one_active_mode_tree(active);
                }
            }
            (effects, receipt)
        };
        if let Some(target) = effects.pane_mode_changed {
            self.sync_automatic_window_name_for_pane_target(&target)
                .await;
            self.emit_without_attached_refresh(LifecycleEvent::PaneModeChanged { target })
                .await;
        }
        for session_name in effects.refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        for error in effects.cleanup_errors {
            tracing::warn!(error = %error, "a mode-tree cleanup was incomplete while leaving every mode of a pane");
        }
        Ok(Some(receipt))
    }

    async fn dismiss_mode_tree_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
        expected_state_id: Option<u64>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let effects = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    expected_attach_id.is_none_or(|expected| active.id == expected)
                        && expected_state_id
                            .is_none_or(|expected| active.mode_tree_state_id == expected)
                        && (expected_attach_id.is_none()
                            || !active.closing.load(std::sync::atomic::Ordering::SeqCst))
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let Some(mode) = active.mode_tree.clone() else {
                return Ok(Vec::new());
            };
            let effects = cleanup_mode_tree_before_removal(&mut state, &mode, true)?;
            clear_active_mode_trees_for_session(
                &mut active_attach,
                &mode.session_name,
                mode.session_id,
            );
            effects
        };
        if let Some(target) = effects.pane_mode_changed {
            self.sync_automatic_window_name_for_pane_target(&target)
                .await;
            self.emit_without_attached_refresh(LifecycleEvent::PaneModeChanged { target })
                .await;
        }
        Ok(effects.refresh_sessions)
    }
}
