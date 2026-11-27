use rmux_proto::RmuxError;

use super::super::attach_support::{ActiveAttach, ActiveAttachIdentity};
use super::super::focus_support::prepare_server_focus_effects;
use super::super::RequestHandler;
use super::menu::{MenuOutcome, OverlayMenuAction};
use super::state::ClientOverlayState;
use crate::pane_io::{AttachControl, OverlayFrame};

impl RequestHandler {
    pub(super) async fn apply_menu_outcome(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        outcome: MenuOutcome,
    ) -> Result<(), RmuxError> {
        if !self
            .overlay_action_is_current(attach_pid, identity)
            .await?
            .is_current()
        {
            return Ok(());
        }
        match outcome {
            MenuOutcome::Stay => {}
            MenuOutcome::Redraw => {
                self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
                    .await?;
            }
            MenuOutcome::Close => {
                let mut clear_root = false;
                {
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach
                        .by_pid
                        .get_mut(&attach_pid)
                        .filter(|active| {
                            identity.is_none_or(|identity| identity.matches_active(active))
                        })
                        .ok_or_else(|| {
                            RmuxError::Server("attached client disappeared".to_owned())
                        })?;
                    match active.overlay.as_mut() {
                        Some(ClientOverlayState::Menu(_)) => clear_root = true,
                        Some(ClientOverlayState::Popup(popup)) => {
                            popup.nested_menu = None;
                            repaint_nested_menu_background(active);
                        }
                        None => {}
                    }
                }
                if clear_root {
                    self.clear_interactive_overlay_for_optional_identity(
                        attach_pid, identity, true,
                    )
                    .await?;
                }
            }
            MenuOutcome::Execute(action) => {
                let ((origin, target, command_context), root_clear, focus_effects) = {
                    let mut state = self.state.lock().await;
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach
                        .by_pid
                        .get_mut(&attach_pid)
                        .filter(|active| {
                            identity.is_none_or(|identity| identity.matches_active(active))
                        })
                        .ok_or_else(|| {
                            RmuxError::Server("attached client disappeared".to_owned())
                        })?;
                    let mut root_clear = None;
                    let action_context = match active.overlay.as_mut() {
                        Some(ClientOverlayState::Menu(menu)) => {
                            let target = menu.current_target.clone();
                            let origin = menu.origin.clone();
                            let command_context = menu.command_context.clone();
                            active.overlay = None;
                            // Removing the menu state must also retire the attach
                            // loop's cached frame and its deferred pane updates.
                            active.overlay_generation = active.overlay_generation.saturating_add(1);
                            root_clear = Some((
                                active.control_tx.clone(),
                                OverlayFrame::persistent(
                                    Vec::new(),
                                    active.render_generation,
                                    active.overlay_generation,
                                ),
                                (active.identity(attach_pid), active.session_name.clone()),
                            ));
                            (origin, target, command_context)
                        }
                        Some(ClientOverlayState::Popup(popup)) => {
                            let Some(menu) = popup.nested_menu.take() else {
                                return Ok(());
                            };
                            repaint_nested_menu_background(active);
                            (menu.origin, menu.current_target, menu.command_context)
                        }
                        None => return Ok(()),
                    };
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    (action_context, root_clear, focus_effects)
                };
                let _ = focus_effects.execute(self).await;
                if let Some((control_tx, clear_frame, restore)) = root_clear {
                    let _ = control_tx.send(AttachControl::Overlay(clear_frame));
                    // Restore output before the command can wait, fail or open
                    // another overlay, and never refresh a replacement client.
                    self.refresh_after_persistent_clear(Some(restore)).await;
                }
                let _access = self.begin_requester_origin_access(&origin);
                match action {
                    OverlayMenuAction::Command(command) => {
                        self.refresh_interactive_overlay_for_optional_identity(
                            attach_pid, identity,
                        )
                        .await
                        .ok();
                        let parsed = self.parse_command_string_one_group(&command).await?;
                        self.execute_menu_command(
                            attach_pid,
                            identity,
                            origin.clone(),
                            parsed,
                            command_context.with_current_target(Some(target)),
                        )
                        .await?;
                    }
                    OverlayMenuAction::Popup(action) => {
                        self.apply_popup_menu_action(attach_pid, identity, action)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn repaint_nested_menu_background(active: &mut ActiveAttach) {
    let Some(overlay) = active.overlay.as_ref() else {
        return;
    };
    let mut frame = overlay.render();
    crate::handler::attach_support::append_transient_message_frame(active, &mut frame);
    active.overlay_generation = active.overlay_generation.saturating_add(1);
    // The removed menu may extend outside the popup. Restore those cells in
    // the same frame as the popup, without dropping its persistent ownership.
    let _ = active.control_tx.send(AttachControl::Overlay(
        OverlayFrame::persistent(frame, active.render_generation, active.overlay_generation)
            .with_deferred_pane_updates()
            .with_background_repaint(),
    ));
}
