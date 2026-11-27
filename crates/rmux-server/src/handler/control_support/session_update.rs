use super::*;
use crate::control::ControlFlagChange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlOutputStart {
    Current,
    Oldest,
}

/// Whether the caller wants `client-session-changed` published at the commit
/// point, before any window geometry the session change reconciles.
///
/// tmux 3.7b, measured 2026-07-25 with two control clients on one session
/// (101x41 and 60x20, `window-size largest`): after the 101x41 client runs
/// `switch-client -t target`, the surviving client receives
///     %client-session-changed client-71338 $1 target
///     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
/// and the switching client receives `%session-changed $1 target` before its
/// own `%layout-change @1 ...`. Both orders come from the same notification, so
/// it has to be published before the reconciles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlSessionChangedNotice {
    Skip,
    Publish,
    PublishIfChanged,
}

pub(super) struct ControlSessionUpdate<'a> {
    target_selection: Option<SwitchTargetSelection>,
    client_environment: Option<&'a HashMap<String, String>>,
    /// `attach-session -r`/`-f` or `switch-client -r` flags this update
    /// applies to the client at the commit point.
    flag_change: ControlFlagChange,
    output_start: ControlOutputStart,
    session_changed_notice: ControlSessionChangedNotice,
}

impl<'a> ControlSessionUpdate<'a> {
    pub(super) fn existing(
        target_selection: Option<SwitchTargetSelection>,
        client_environment: Option<&'a HashMap<String, String>>,
    ) -> Self {
        Self {
            target_selection,
            client_environment,
            flag_change: ControlFlagChange::Keep,
            output_start: ControlOutputStart::Current,
            session_changed_notice: ControlSessionChangedNotice::Skip,
        }
    }

    fn switched(
        target_selection: Option<SwitchTargetSelection>,
        client_environment: Option<&'a HashMap<String, String>>,
    ) -> Self {
        Self {
            session_changed_notice: ControlSessionChangedNotice::Publish,
            ..Self::existing(target_selection, client_environment)
        }
    }

    pub(super) fn attached_existing() -> Self {
        Self {
            session_changed_notice: ControlSessionChangedNotice::PublishIfChanged,
            ..Self::existing(None, None)
        }
    }

    pub(super) fn created() -> Self {
        Self {
            target_selection: None,
            client_environment: None,
            flag_change: ControlFlagChange::Keep,
            output_start: ControlOutputStart::Oldest,
            session_changed_notice: ControlSessionChangedNotice::Skip,
        }
    }
}

impl RequestHandler {
    /// `switch-client` for a control client: same commit as
    /// the regular identity-checked control-session update, with the
    /// `client-session-changed` notification published at the commit point so
    /// it precedes the `%layout-change` lines the reconciles produce.
    pub(in crate::handler) async fn switch_control_session_for_client_identity(
        &self,
        identity: ControlClientIdentity,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: SessionId,
        target_selection: Option<SwitchTargetSelection>,
        client_environment: Option<&HashMap<String, String>>,
        flag_change: ControlFlagChange,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            identity.requester_pid(),
            Some(next_session_name),
            Some(expected_session_id),
            Some(identity.control_id()),
            ControlSessionUpdate {
                flag_change,
                ..ControlSessionUpdate::switched(target_selection, client_environment)
            },
        )
        .await
    }

    pub(super) async fn set_control_session_with_expected_identity(
        &self,
        requester_pid: u32,
        next_session_name: Option<rmux_proto::SessionName>,
        expected_session_id: Option<SessionId>,
        expected_control_id: Option<u64>,
        update: ControlSessionUpdate<'_>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        let ControlSessionUpdate {
            target_selection,
            client_environment,
            output_start,
            flag_change,
            session_changed_notice,
        } = update;
        let exact_client_identity = expected_control_id.is_some();
        let command_name = if exact_client_identity {
            "switch-client"
        } else {
            "control session"
        };
        let touch_attached = exact_client_identity;
        let queued_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        if expected_control_id
            .zip(queued_control_id)
            .is_some_and(|(explicit, queued)| explicit != queued)
        {
            return Err(attached_client_required(command_name));
        }
        let expected_control_id = expected_control_id.or(queued_control_id);
        let mut state = self.state.lock().await;
        let next_session_id = match next_session_name.as_ref() {
            Some(session_name) => {
                let session_id = state
                    .sessions
                    .session(session_name)
                    .ok_or_else(|| {
                        rmux_proto::RmuxError::SessionNotFound(session_name.to_string())
                    })?
                    .id();
                if expected_session_id.is_some_and(|expected| expected != session_id) {
                    return Err(rmux_proto::RmuxError::SessionNotFound(
                        session_name.to_string(),
                    ));
                }
                Some(session_id)
            }
            None => None,
        };
        let pane_sequences = match output_start {
            ControlOutputStart::Current => next_session_name
                .as_ref()
                .map(|session_name| current_pane_output_sequences(&state, session_name))
                .transpose()?,
            ControlOutputStart::Oldest => None,
        };
        let active_attach = self.active_attach.lock().await;
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control.by_pid.get(&requester_pid) else {
            return Err(attached_client_required(command_name));
        };
        if expected_control_id.is_some_and(|expected| active.id != expected)
            || active.closing.load(Ordering::SeqCst)
        {
            return Err(attached_client_required(command_name));
        }
        if let Some(selection) = target_selection.as_ref() {
            let session_name = next_session_name
                .as_ref()
                .expect("a switch target selection carries a session");
            let session_id = next_session_id
                .expect("a switch target selection carries a stable session identity");
            selection.validate_for_session_identity(&state, session_name, session_id)?;
        }
        let selection_transition = target_selection
            .as_ref()
            .map(|selection| {
                SelectionTargetTransitionSnapshot::capture(&state, selection.window_target())
            })
            .transpose()?;
        let mut next_flags = active.flags;
        flag_change.apply(&mut next_flags);
        let control_size = ((active.size_declared || active.window_sizes.declared())
            && !next_flags.ignore_size)
            .then_some(TerminalSize {
                cols: active.client_width,
                rows: active.client_height,
            });
        let session_changed =
            active.session_name != next_session_name || active.session_id != next_session_id;
        let previous_session_id = active.session_id;
        let size_sequence = next_session_name
            .as_ref()
            .map(|_| self.next_client_size_sequence());
        let prepared_update =
            prepare_control_session_update(active, next_session_name.clone(), pane_sequences)
                .ok_or_else(|| attached_client_required(command_name))?;
        let refresh_sessions = if exact_client_identity {
            let (session_name, session_id) = next_session_name
                .as_ref()
                .zip(next_session_id)
                .expect("switch-client always carries a target session identity");
            switch_control_session_for_client(
                &mut state,
                ControlResizeClient::new(
                    &active_attach,
                    &active_control,
                    requester_pid,
                    control_size,
                    size_sequence.expect("switch-client carries a target session"),
                ),
                session_name,
                session_id,
                target_selection.as_ref(),
            )?
        } else {
            Vec::new()
        };
        if let (Some(session_name), Some(client_environment)) =
            (next_session_name.as_ref(), client_environment)
        {
            update_environment_from_client(&mut state, session_name, client_environment);
        }
        let active = active_control
            .by_pid
            .get_mut(&requester_pid)
            .expect("validated control client remains locked");
        // Apply the requested flags only after all fallible target and geometry
        // work succeeds. The client, sessions and target stayed locked, and the
        // size selection above already used this prospective eligibility.
        active.flags = next_flags;
        let previous = commit_control_session_update(
            active,
            next_session_name.clone(),
            next_session_id,
            prepared_update,
        );
        if let Some(size_sequence) = size_sequence {
            active.size_sequence = size_sequence;
        }
        if touch_attached {
            let session_name = next_session_name
                .as_ref()
                .expect("switch-client always carries a target session");
            state
                .sessions
                .touch_attached(session_name)
                .expect("target session stayed locked across the control update");
        }
        let selection_events = selection_transition
            .map(|transition| transition.prepare(&mut state))
            .unwrap_or_default();
        drop(active_control);
        drop(active_attach);
        drop(state);
        for event in selection_events {
            self.emit_prepared(event).await;
        }
        if session_changed_notice == ControlSessionChangedNotice::Publish
            || (session_changed_notice == ControlSessionChangedNotice::PublishIfChanged
                && session_changed)
        {
            if let (Some(session_name), Some(session_id)) =
                (next_session_name.as_ref(), next_session_id)
            {
                self.emit_client_session_changed(
                    control_client_name(requester_pid),
                    session_name.clone(),
                    session_id,
                )
                .await;
            }
        }
        // The destination resize was recorded by the geometry chokepoint and is
        // published by `emit_client_session_changed` above (or by the dispatch
        // backstop when this update carries no session-changed notice).
        self.publish_applied_window_resizes().await;
        self.refresh_linked_window_sessions(refresh_sessions).await;
        if let Some(session_id) = next_session_id {
            self.reconcile_attached_session_identity_size_and_emit(session_id)
                .await?;
        }
        if let Some(previous_session_id) =
            previous_session_id.filter(|previous| Some(*previous) != next_session_id)
        {
            let _ = self
                .reconcile_attached_session_identity_size_and_emit(previous_session_id)
                .await;
            self.destroy_unattached_session_identity(previous_session_id)
                .await;
        }
        Ok(previous)
    }
}
