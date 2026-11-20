use rmux_proto::RmuxError;

use super::super::attach_support::ActiveAttachIdentity;
use super::super::focus_support::prepare_server_focus_effects;
use super::super::RequestHandler;
use super::ClientOverlayState;
use crate::pane_io::{AttachControl, OverlayFrame};

impl RequestHandler {
    pub(in crate::handler) async fn popup_reader_tick(
        &self,
        identity: ActiveAttachIdentity,
        popup_id: u64,
    ) -> Result<(), RmuxError> {
        if !self
            .overlay_action_is_current(identity.attach_pid(), Some(identity))
            .await?
            .is_current()
        {
            return Ok(());
        }
        let surface = {
            let active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get(&identity.attach_pid()) else {
                return Ok(());
            };
            if !identity.matches_active(active)
                || active.closing.load(std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(());
            }
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
                return Ok(());
            };
            if popup.id != popup_id {
                return Ok(());
            }
            popup.surface.clone()
        };
        if !surface.lock().expect("popup surface").mark_reader_dirty() {
            return Ok(());
        }
        let handler = self.clone();
        tokio::spawn(async move {
            // Let already-ready reader blocks invalidate the same surface.
            // Neither the attach nor the surface mutex crosses a yield/gate.
            tokio::task::yield_now().await;
            #[cfg(test)]
            {
                let gate = surface
                    .lock()
                    .expect("popup surface")
                    .flush_gate_for_test
                    .clone();
                if let Some(gate) = gate {
                    gate.acquire().await.expect("popup flush gate").forget();
                }
            }
            loop {
                surface.lock().expect("popup surface").begin_reader_flush();
                let current = handler
                    .overlay_action_is_current(identity.attach_pid(), Some(identity))
                    .await
                    .is_ok_and(|status| status.is_current());
                if current {
                    let _ = handler
                        .refresh_popup_overlay_for_identity(identity, popup_id)
                        .await;
                }
                let repeat = surface
                    .lock()
                    .expect("popup surface")
                    .finish_reader_flush(current);
                if !repeat {
                    break;
                }
                tokio::task::yield_now().await;
            }
            #[cfg(test)]
            surface
                .lock()
                .expect("popup surface")
                .flush_done_for_test
                .notify_one();
        });
        Ok(())
    }

    /// The popup's job terminal reported the end of its output. Wakes the job's exit waiter, so a
    /// popup closing with its job closes when the job exits, as in tmux, instead of taking the keys
    /// meant for what is under it until the waiter's next look.
    #[cfg(unix)]
    pub(in crate::handler) async fn popup_output_closed(
        &self,
        identity: ActiveAttachIdentity,
        popup_id: u64,
    ) {
        let active_attach = self.active_attach.lock().await;
        let job = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| identity.matches_active(active))
            .and_then(|active| match active.overlay.as_ref() {
                Some(ClientOverlayState::Popup(popup)) if popup.id == popup_id => {
                    popup.job.as_ref()
                }
                _ => None,
            });
        if let Some(job) = job {
            job.output_closed();
        }
    }

    pub(in crate::handler) async fn popup_job_finished(
        &self,
        identity: ActiveAttachIdentity,
        popup_id: u64,
        status: i32,
    ) -> Result<(), RmuxError> {
        if !self
            .overlay_action_is_current(identity.attach_pid(), Some(identity))
            .await?
            .is_current()
        {
            return Ok(());
        }
        let clear = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&identity.attach_pid()) else {
                return Ok(());
            };
            if !identity.matches_active(active) {
                return Ok(());
            }
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
                return Ok(());
            };
            if popup.id != popup_id {
                return Ok(());
            }
            popup.job = None;
            if let Some(completion) = popup.completion.as_ref() {
                completion.record_job_exit(status);
            }
            let should_close = popup.close_on_exit || (popup.close_on_zero_exit && status == 0);
            if !should_close {
                None
            } else {
                let _ = active.overlay.take();
                active.overlay_generation = active.overlay_generation.saturating_add(1);
                let clear = (
                    active.control_tx.clone(),
                    active.render_generation,
                    active.overlay_generation,
                    Some((
                        active.identity(identity.attach_pid()),
                        active.session_name.clone(),
                    )),
                );
                let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
                Some((clear, focus_effects))
            }
        };
        if let Some((
            (control_tx, render_generation, overlay_generation, transient_restore),
            focus_effects,
        )) = clear
        {
            let _ = focus_effects.execute(self).await;
            let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::persistent(
                Vec::new(),
                render_generation,
                overlay_generation,
            )));
            self.refresh_after_persistent_clear(transient_restore).await;
        } else {
            self.refresh_popup_overlay_for_identity(identity, popup_id)
                .await?;
        }
        Ok(())
    }

    async fn refresh_popup_overlay_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        popup_id: u64,
    ) -> Result<(), RmuxError> {
        loop {
            let (overlay, render_generation, overlay_generation) = {
                let active_attach = self.active_attach.lock().await;
                let Some(active) = active_attach.by_pid.get(&identity.attach_pid()) else {
                    return Ok(());
                };
                if !identity.matches_active(active)
                    || active.closing.load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Ok(());
                }
                let Some(overlay) = active.overlay.clone() else {
                    return Ok(());
                };
                if overlay.id() != popup_id {
                    return Ok(());
                }
                (overlay, active.render_generation, active.overlay_generation)
            };

            let mut frame = overlay.render();
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&identity.attach_pid()) else {
                return Ok(());
            };
            if !identity.matches_active(active)
                || active.closing.load(std::sync::atomic::Ordering::SeqCst)
                || active
                    .overlay
                    .as_ref()
                    .is_none_or(|current| current.id() != popup_id)
            {
                return Ok(());
            }
            if active.render_generation != render_generation
                || active.overlay_generation != overlay_generation
            {
                // A resize/replay/final flush published while rendering. Never
                // stamp obsolete contents with the newer publication generation.
                drop(active_attach);
                tokio::task::yield_now().await;
                continue;
            }
            crate::handler::attach_support::append_transient_message_frame(active, &mut frame);
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            let sent = active.control_tx.send_popup_overlay(
                popup_id,
                OverlayFrame::persistent(
                    frame,
                    active.render_generation,
                    active.overlay_generation,
                )
                .with_deferred_pane_updates()
                .with_row_diff(),
            );
            #[cfg(test)]
            if sent.is_ok() {
                if let ClientOverlayState::Popup(popup) = &overlay {
                    popup
                        .surface
                        .lock()
                        .expect("popup surface")
                        .record_publication_for_test();
                }
            }
            let _ = sent;
            return Ok(());
        }
    }
}
