use rmux_core::{TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget, Window};
use rmux_proto::{PaneTarget, RmuxError, SessionName, Target};

use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use super::mode_tree_model::{ModeTreeActionIdentity, ModeTreeClientState, ModeTreePaneIdentity};
use crate::handler_support::attached_client_required;
use crate::pane_terminals::session_not_found;

impl RequestHandler {
    pub(super) async fn current_mode_tree_action_identity(
        &self,
        attach_pid: u32,
    ) -> Result<ModeTreeActionIdentity, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .filter(|active| {
                active.mode_tree.is_some()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?;
        Ok(ModeTreeActionIdentity::new(
            attach_pid,
            active.id,
            active.mode_tree_state_id,
        ))
    }

    pub(super) async fn capture_mode_tree_host_identity(
        &self,
        target: &PaneTarget,
    ) -> Result<ModeTreePaneIdentity, RmuxError> {
        let mut state = self.state.lock().await;
        ModeTreePaneIdentity::capture(&mut state, target)
    }

    pub(super) async fn capture_mode_tree_host(
        &self,
        target: &PaneTarget,
    ) -> Result<
        (
            ModeTreePaneIdentity,
            crate::pane_transcript::SharedPaneTranscript,
        ),
        RmuxError,
    > {
        let mut state = self.state.lock().await;
        let identity = ModeTreePaneIdentity::capture(&mut state, target)?;
        let transcript = state.transcript_handle(target)?;
        Ok((identity, transcript))
    }

    pub(super) async fn mode_tree_attach_identity(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<ActiveAttachIdentity, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        if let Some(active) = active_attach.by_pid.get(&requester_pid) {
            return Ok(active.identity(requester_pid));
        }
        active_attach
            .by_pid
            .iter()
            .min_by_key(|(_, active)| active.id)
            .map(|(&pid, active)| active.identity(pid))
            .ok_or_else(|| attached_client_required(command_name))
    }

    pub(super) async fn detached_mode_tree_target(
        &self,
        target: Option<&str>,
    ) -> Result<PaneTarget, RmuxError> {
        let state = self.state.lock().await;
        let unresolved = target
            .map(|target| UnresolvedTarget::new(target.to_owned()))
            .unwrap_or_else(UnresolvedTarget::none);
        let resolved = state.sessions.resolve_unresolved_target(
            &unresolved,
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &TargetFindContext::new(None),
        )?;
        match resolved {
            Target::Pane(target) => Ok(target),
            Target::Session(_) | Target::Window(_) => Err(RmuxError::Server(
                "mode tree target did not resolve to a pane".to_owned(),
            )),
        }
    }

    pub(super) async fn mode_tree_for_action_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<ModeTreeClientState, RmuxError> {
        let state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let mode = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                active.id == identity.attach_id()
                    && active.mode_tree_state_id == identity.state_id()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .and_then(|active| active.mode_tree.as_ref())
            .filter(|mode| mode_tree_host_is_current(&state, mode))
            .cloned()
            .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?;
        Ok(mode)
    }

    pub(super) async fn activate_mode_tree_for_session(
        &self,
        requester: ActiveAttachIdentity,
        mode: &mut ModeTreeClientState,
        zoom_requested: bool,
    ) -> Result<(Vec<SessionName>, bool), RmuxError> {
        let mut state = self.state.lock().await;
        if !mode_tree_host_is_current(&state, mode) {
            return Err(RmuxError::Server(
                "mode-tree host identity changed before activation".to_owned(),
            ));
        }
        let host_transcript = mode
            .host_identity
            .as_ref()
            .zip(mode.host_transcript.as_ref())
            .map(|(identity, transcript)| (identity.target().clone(), transcript.clone()));

        let mut active_attach = self.active_attach.lock().await;
        let requester_is_current = active_attach
            .by_pid
            .get(&requester.attach_pid())
            .is_some_and(|active| {
                requester.matches_active_session(active, &mode.session_name, mode.session_id)
                    && !active.suspended
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            });
        if !requester_is_current {
            return Err(attached_client_required(mode.kind.command_name()));
        }

        let pane_mode_changed = if let Some((target, transcript)) = host_transcript.as_ref() {
            let changed = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .enter_mode_tree(mode.kind.pane_mode_name());
            if changed {
                if let Err(error) = state.resize_terminals(target.session_name()) {
                    transcript
                        .lock()
                        .expect("pane transcript mutex must not be poisoned")
                        .clear_mode_tree();
                    let _ = state.resize_terminals(target.session_name());
                    return Err(error);
                }
            }
            changed
        } else {
            false
        };

        let refresh_sessions = if zoom_requested {
            let session = state
                .sessions
                .session(&mode.session_name)
                .filter(|session| session.id() == mode.session_id)
                .ok_or_else(|| session_not_found(&mode.session_name))?;
            let window_index = session.active_window_index();
            let pane_index = session.active_pane_index();
            if session
                .window_at(window_index)
                .is_some_and(Window::is_zoomed)
            {
                Vec::new()
            } else {
                let target =
                    PaneTarget::with_window(mode.session_name.clone(), window_index, pane_index);
                let zoom_restore = ModeTreePaneIdentity::capture(&mut state, &target)?;
                let refresh_sessions = match state
                    .mutate_session_and_resize_window_terminal_with_family(
                        &mode.session_name,
                        window_index,
                        |session| {
                            session.toggle_zoom_in_window(window_index, pane_index)?;
                            Ok(())
                        },
                    ) {
                    Ok(((), refresh_sessions)) => refresh_sessions,
                    Err(error) => {
                        if pane_mode_changed {
                            if let Some((target, transcript)) = host_transcript.as_ref() {
                                transcript
                                    .lock()
                                    .expect("pane transcript mutex must not be poisoned")
                                    .clear_mode_tree();
                                let _ = state.resize_terminals(target.session_name());
                            }
                        }
                        return Err(error);
                    }
                };
                mode.zoom_restore = Some(zoom_restore);
                refresh_sessions
            }
        } else {
            Vec::new()
        };

        for active in active_attach.by_pid.values_mut() {
            if active.session_name != mode.session_name
                || active.session_id != mode.session_id
                || active.suspended
                || active.closing.load(std::sync::atomic::Ordering::SeqCst)
            {
                continue;
            }
            active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
            active.persistent_overlay_epoch.store(
                active.mode_tree_state_id,
                std::sync::atomic::Ordering::SeqCst,
            );
            active.mode_tree = Some(mode.clone());
        }
        Ok((refresh_sessions, pane_mode_changed))
    }
}

pub(super) fn mode_tree_host_is_current(
    state: &crate::pane_terminals::HandlerState,
    mode: &ModeTreeClientState,
) -> bool {
    state
        .sessions
        .session(&mode.session_name)
        .is_some_and(|session| session.id() == mode.session_id)
        && mode
            .host_identity
            .as_ref()
            .is_none_or(|identity| identity.matches(state))
}
