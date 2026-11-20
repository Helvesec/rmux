//! Rebasing queued command targets and pinned identities across a session rename.

use std::sync::Arc;

use rmux_proto::{PaneTarget, SessionId, SessionName, Target, WindowTarget};

use super::QueueExecutionContext;

impl QueueExecutionContext {
    pub(in crate::handler) fn rename_session_targets(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
    ) {
        if let Some(target) = self.current_target.as_mut() {
            rename_target_session(target, old_name, new_name);
        }
        if let Some(target) = self.mouse_target.as_mut() {
            rename_target_session(target, old_name, new_name);
        }
        if let Some(event) = self.mouse_event.as_mut() {
            if let Some(target) = event.pane_target.as_mut() {
                rename_pane_target_session(target, old_name, new_name);
            }
        }
        if let Some(identity) = self.pinned_current_target_identity.as_mut() {
            Arc::make_mut(identity).rename_session(old_name, new_name);
        }
        if let Some(identity) = self.pinned_pane_output_identity.as_mut() {
            Arc::make_mut(identity).rename_session(old_name, new_name);
        }
        if self
            .rebased_current_session
            .as_ref()
            .is_some_and(|(_, session_name)| session_name == old_name)
        {
            self.rebased_current_session = self
                .rebased_current_session
                .take()
                .map(|(id, _)| (id, new_name.clone()));
        }
    }

    pub(in crate::handler::scripting_support) fn rebase_for_source_validation_session_rename(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
    ) {
        if self.current_target.is_none() && self.source_file_depth > 0 {
            self.current_target = Some(Target::Session(new_name.clone()));
            return;
        }
        self.rename_session_targets(old_name, new_name);
    }

    pub(in crate::handler::scripting_support) fn rebase_after_session_rename(
        &mut self,
        session_id: SessionId,
        old_name: &SessionName,
        new_name: &SessionName,
    ) {
        if self
            .pinned_current_target_identity
            .as_ref()
            .is_some_and(|identity| identity.session_id() != session_id)
        {
            return;
        }
        if self.current_target.is_none() && self.source_file_depth > 0 {
            self.current_target = Some(Target::Session(new_name.clone()));
            self.rebased_current_session = Some((session_id, new_name.clone()));
            return;
        }
        let rebases_current_target = self
            .current_target
            .as_ref()
            .is_some_and(|target| target.session_name() == old_name);
        self.rename_session_targets(old_name, new_name);
        if rebases_current_target {
            self.rebased_current_session = Some((session_id, new_name.clone()));
        }
    }
}

pub(in crate::handler) fn rename_target_session(
    target: &mut Target,
    old_name: &SessionName,
    new_name: &SessionName,
) {
    match target {
        Target::Session(session_name) if session_name == old_name => {
            *session_name = new_name.clone();
        }
        Target::Window(window) => rename_window_target_session(window, old_name, new_name),
        Target::Pane(pane) => rename_pane_target_session(pane, old_name, new_name),
        Target::Session(_) => {}
    }
}

pub(in crate::handler) fn rename_window_target_session(
    target: &mut WindowTarget,
    old_name: &SessionName,
    new_name: &SessionName,
) {
    if target.session_name() != old_name {
        return;
    }
    *target = WindowTarget::with_window(new_name.clone(), target.window_index());
}

pub(in crate::handler) fn rename_pane_target_session(
    target: &mut PaneTarget,
    old_name: &SessionName,
    new_name: &SessionName,
) {
    if target.session_name() != old_name {
        return;
    }
    *target = PaneTarget::with_window(new_name.clone(), target.window_index(), target.pane_index());
}
