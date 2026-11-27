use std::io;
use std::time::Instant;

use rmux_proto::{OptionName, PaneTarget, Target};

use super::super::pane_io_encoding::{
    encode_mouse_for_target, prepare_attached_pane_input_writes, write_attached_bytes_to_target_io,
};
use super::ensure_session_identity;
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::pane_support::{io_other, AttachedKeyDispatch};
use crate::handler::RequestHandler;
use crate::mouse::{classify_mouse_events, mouse_event_for_pane_passthrough, ClassifiedMouseEvent};

impl RequestHandler {
    pub(super) async fn handle_attached_live_mouse(
        &self,
        identity: ActiveAttachIdentity,
        raw: crate::input_keys::MouseForwardEvent,
    ) -> io::Result<()> {
        let attach_pid = identity.attach_pid();
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            return Ok(());
        }
        let (session_name, session_id) = self
            .attached_session_identity_for_identity(identity)
            .await
            .map_err(io_other)?;
        let mouse_enabled = {
            let state = self.state.lock().await;
            ensure_session_identity(&state, &session_name, session_id).map_err(io_other)?;
            matches!(
                state
                    .options
                    .resolve(Some(&session_name), OptionName::Mouse),
                Some("on")
            )
        };
        if self.mode_tree_active_for_identity(identity).await {
            if mouse_enabled {
                let _ = self
                    .handle_mode_tree_mouse_event_for_identity(identity, raw)
                    .await
                    .map_err(io_other)?;
            }
            return Ok(());
        }
        let attached_count = self
            .attached_count_for_session_identity(&session_name, session_id)
            .await;
        let layout = self.attached_mouse_layout(identity, attached_count).await;
        let Some(layout) = layout else {
            return Ok(());
        };
        if !mouse_enabled {
            let Some(passthrough) = mouse_event_for_pane_passthrough(&layout, raw) else {
                return Ok(());
            };
            if let (Some(window_id), Some(pane_id)) =
                (passthrough.event.window_id, passthrough.focus_target)
            {
                self.select_attached_mouse_focus(
                    identity,
                    &session_name,
                    session_id,
                    window_id,
                    pane_id,
                )
                .await?;
            }
            let Some(target) = passthrough.event.pane_target.clone() else {
                return Ok(());
            };
            self.forward_attached_mouse_event_to_pane_for_session_identity(
                identity,
                session_id,
                &target,
                &passthrough.event,
            )
            .await?;
            return Ok(());
        }
        let (classified, click_timer) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    identity.matches_active_session(active, &session_name, session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| io_other("attached client disappeared"))?;
            let now = Instant::now();
            #[cfg(test)]
            let now = self.attached_mouse_click_instant_for_test(now);
            let classified = classify_mouse_events(&mut active.mouse, &layout, raw, now);
            (classified, active.mouse.click_timer_token())
        };
        if let Some(token) = click_timer {
            self.schedule_attached_mouse_click_timer(identity, token);
        }
        if classified.is_empty() {
            return Ok(());
        }
        for classified in classified {
            self.dispatch_attached_mouse_classified(
                identity,
                &session_name,
                session_id,
                classified,
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn dispatch_attached_mouse_classified(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        classified: ClassifiedMouseEvent,
    ) -> io::Result<()> {
        let attach_pid = identity.attach_pid();
        if let (Some(window_id), Some(pane_id)) =
            (classified.event.window_id, classified.focus_target)
        {
            self.select_attached_mouse_focus(
                identity,
                session_name,
                session_id,
                window_id,
                pane_id,
            )
            .await?;
        }
        let target = if let Some(target) = classified.event.pane_target.clone() {
            target
        } else {
            let (target, current_session_id) = self
                .attached_input_target_identity(identity)
                .await
                .map_err(io_other)?;
            if current_session_id != session_id || target.session_name() != session_name {
                return Ok(());
            }
            target
        };
        if target.session_name() != session_name {
            return Ok(());
        }
        let current_target = self
            .attached_mouse_target_for_session_identity(
                identity,
                session_name,
                session_id,
                &classified.event,
            )
            .await
            .map_err(io_other)?
            .or_else(|| Some(Target::Pane(target.clone())));
        let mouse_target = current_target.clone();
        let handled = self
            .dispatch_attached_key_inner(
                &target,
                AttachedKeyDispatch {
                    attach_pid,
                    live_identity: Some(identity),
                    live_session_id: Some(session_id),
                    requester_pid: attach_pid,
                    current_target,
                    mouse_target,
                    mouse_event: Some(classified.event.clone()),
                    key: classified.key,
                    attached_live_input: true,
                },
            )
            .await
            .map_err(io_other)?;
        if !handled {
            self.forward_attached_mouse_event_to_pane_for_session_identity(
                identity,
                session_id,
                &target,
                &classified.event,
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn forward_attached_mouse_event_to_pane_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_id: rmux_proto::SessionId,
        target: &PaneTarget,
        event: &crate::mouse::AttachedMouseEvent,
    ) -> io::Result<bool> {
        let Some(prepared) = self
            .with_live_input_state(identity, target, session_id, |state| {
                let bytes = encode_mouse_for_target(state, target, event).map_err(io_other)?;
                if bytes.is_empty() {
                    return Ok(None);
                }
                let writes =
                    prepare_attached_pane_input_writes(state, target, &bytes).map_err(io_other)?;
                Ok(Some((writes, bytes)))
            })
            .await?
            .flatten()
        else {
            return Ok(false);
        };

        for write in prepared.0 {
            write_attached_bytes_to_target_io(write, prepared.1.clone())
                .await
                .map_err(io_other)?;
        }
        Ok(true)
    }

    pub(in crate::handler) async fn attached_last_mouse_event(
        &self,
        attach_pid: u32,
    ) -> Option<crate::input_keys::MouseForwardEvent> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .and_then(|active| active.mouse.current_event.as_ref().map(|event| event.raw))
    }

    pub(in crate::handler) async fn attached_last_mouse_event_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Option<crate::input_keys::MouseForwardEvent> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .and_then(|active| active.mouse.current_event.as_ref().map(|event| event.raw))
    }
}
