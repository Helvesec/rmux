use rmux_core::LifecycleEvent;
use rmux_proto::{RmuxError, TerminalGeometry, TerminalSize};

use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use super::ClientOverlayState;

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn handle_attached_resize(
        &self,
        attach_pid: u32,
        size: TerminalSize,
    ) -> Result<(), RmuxError> {
        self.handle_attached_resize_geometry_with_identity(
            attach_pid,
            None,
            TerminalGeometry::from_size(size),
        )
        .await
    }

    pub(crate) async fn handle_attached_resize_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        size: TerminalSize,
    ) -> Result<(), RmuxError> {
        self.handle_attached_resize_geometry_for_identity(
            identity,
            TerminalGeometry::from_size(size),
        )
        .await
    }

    pub(crate) async fn handle_attached_resize_geometry_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        geometry: TerminalGeometry,
    ) -> Result<(), RmuxError> {
        self.handle_attached_resize_geometry_with_identity(
            identity.attach_pid(),
            Some(identity),
            geometry,
        )
        .await
    }

    async fn handle_attached_resize_geometry_with_identity(
        &self,
        attach_pid: u32,
        expected_identity: Option<ActiveAttachIdentity>,
        geometry: TerminalGeometry,
    ) -> Result<(), RmuxError> {
        let size = geometry.size;
        if size.cols == 0 || size.rows == 0 {
            return Ok(());
        }

        let mut close_overlay = false;
        let mut popup_resize = None;
        let (resized_session, resized_session_id, client_name, ignores_size, client_size_changed) = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get(&attach_pid).filter(|active| {
                expected_identity.is_none_or(|identity| {
                    identity.matches_active(active)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
            }) else {
                return Ok(());
            };
            let ignores_size = active
                .flags
                .contains(crate::handler::attach_support::ClientFlags::IGNORESIZE);
            let client_size_changed = active.client_size != size;
            // A real resize promotes an inferred anchor to declared geometry
            // even when the dimensions match it, so the declaration takes its
            // place in `window-size latest` ordering.
            let geometry_changed = client_size_changed
                || active.client_pixels != geometry.pixels
                || active.client_size_is_inferred();
            let size_sequence = geometry_changed.then(|| self.next_client_size_sequence());
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .expect("attached client checked above");
            if let Some(size_sequence) = size_sequence {
                active.set_declared_client_size(size);
                active.client_pixels = geometry.pixels;
                active.size_sequence = size_sequence;
            }
            let session_name = active.session_name.clone();
            if let Some(overlay) = active.overlay.as_mut() {
                match overlay {
                    ClientOverlayState::Menu(menu) => {
                        if size.cols == 0 || size.rows == 0 {
                            close_overlay = true;
                        } else {
                            menu.rect.width = menu.rect.width.min(size.cols);
                            menu.rect.height = menu.rect.height.min(size.rows);
                            menu.rect.x =
                                menu.rect.x.min(size.cols.saturating_sub(menu.rect.width));
                            menu.rect.y =
                                menu.rect.y.min(size.rows.saturating_sub(menu.rect.height));
                        }
                    }
                    ClientOverlayState::Popup(popup) => {
                        if size.cols == 0 || size.rows == 0 {
                            close_overlay = true;
                        } else {
                            popup.rect.width = popup.preferred_width.min(size.cols);
                            popup.rect.height = popup.preferred_height.min(size.rows);
                            popup.rect.x =
                                popup.rect.x.min(size.cols.saturating_sub(popup.rect.width));
                            popup.rect.y = popup
                                .rect
                                .y
                                .min(size.rows.saturating_sub(popup.rect.height));
                            let content_size = popup.content_size();
                            popup
                                .surface
                                .lock()
                                .expect("popup surface")
                                .resize(content_size);
                            popup_resize = popup
                                .job
                                .as_ref()
                                .and_then(|job| job.enqueue_resize(content_size).ok());
                            if let Some(menu) = popup.nested_menu.as_mut() {
                                menu.rect.width = menu.rect.width.min(size.cols);
                                menu.rect.height = menu.rect.height.min(size.rows);
                                menu.rect.x =
                                    menu.rect.x.min(size.cols.saturating_sub(menu.rect.width));
                                menu.rect.y =
                                    menu.rect.y.min(size.rows.saturating_sub(menu.rect.height));
                            }
                        }
                    }
                }
            }
            (
                session_name,
                active.session_id,
                active.client_name.clone(),
                ignores_size,
                client_size_changed,
            )
        };
        if let Some(receipt) = popup_resize {
            let _ = receipt.wait().await;
        }

        let size_policy = match expected_identity {
            Some(_) => {
                self.attached_window_size_policy_for_session_identity(
                    &resized_session,
                    resized_session_id,
                )
                .await?
            }
            None => {
                self.attached_window_size_policy_for_session(&resized_session)
                    .await?
            }
        };
        if !ignores_size
            && size_policy != crate::handler::attach_support::AttachedWindowSizePolicy::Manual
        {
            let mut state = self.state.lock().await;
            if let Some(identity) = expected_identity {
                let active_attach = self.active_attach.lock().await;
                let current = active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                    identity.matches_active_session(active, &resized_session, resized_session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                });
                let session_current = state
                    .sessions
                    .session(&resized_session)
                    .is_some_and(|session| session.id() == resized_session_id);
                if !current || !session_current {
                    return Ok(());
                }
            }
            state.set_attached_terminal_geometry(&resized_session, geometry);
        }
        // Reconcile before the resize redraw, using the same capacity as input
        // and rendering. Keep the initial client-size capture independent of state.
        {
            let state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            if state
                .sessions
                .session(&resized_session)
                .is_some_and(|session| session.id() == resized_session_id)
            {
                if let Some(active) = active_attach.by_pid.get_mut(&attach_pid).filter(|active| {
                    active.session_name == resized_session
                        && active.session_id == resized_session_id
                        && expected_identity.is_none_or(|identity| identity.matches_active(active))
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                }) {
                    if let Some(prompt) = active.prompt.as_mut() {
                        let status = state
                            .options
                            .resolve(Some(&resized_session), rmux_proto::OptionName::Status);
                        prompt.reconcile_completion_menu(
                            crate::status_lines::prompt_completion_menu_height(
                                active.client_size,
                                crate::status_lines::status_line_count(
                                    status,
                                    active.client_size.rows,
                                ),
                            ),
                        );
                    }
                }
            }
        }
        if let Some(identity) = expected_identity {
            let current = {
                let active_attach = self.active_attach.lock().await;
                active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                    identity.matches_active_session(active, &resized_session, resized_session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
            };
            if !current {
                return Ok(());
            }
            self.reconcile_attached_session_identity_size_and_emit(resized_session_id)
                .await?;
        } else {
            self.reconcile_attached_session_size_and_emit(&resized_session)
                .await?;
        }
        if client_size_changed {
            let event = LifecycleEvent::ClientResized {
                session_name: resized_session.clone(),
                client_name: Some(client_name),
            };
            if let Some(identity) = expected_identity {
                let prepared = {
                    let mut state = self.state.lock().await;
                    let active_attach = self.active_attach.lock().await;
                    let current = active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                        identity.matches_active_session(
                            active,
                            &resized_session,
                            resized_session_id,
                        ) && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    });
                    if !current
                        || state
                            .sessions
                            .session(&resized_session)
                            .is_none_or(|session| session.id() != resized_session_id)
                    {
                        return Ok(());
                    }
                    crate::handler::prepare_lifecycle_event(&mut state, &event)
                };
                self.emit_prepared(prepared).await;
            } else {
                self.emit(event).await;
            }
        }
        if let Some(identity) = expected_identity {
            let current = {
                let active_attach = self.active_attach.lock().await;
                active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                    identity.matches_active_session(active, &resized_session, resized_session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
            };
            if current {
                self.refresh_attached_session_for_session_identity(
                    &resized_session,
                    resized_session_id,
                )
                .await;
            }
        } else {
            self.refresh_attached_session(&resized_session).await;
        }

        if let Some(identity) = expected_identity {
            if close_overlay {
                self.clear_interactive_overlay_for_session_identity(
                    identity,
                    &resized_session,
                    resized_session_id,
                    true,
                )
                .await?;
            } else {
                self.refresh_interactive_overlay_for_session_identity(
                    identity,
                    &resized_session,
                    resized_session_id,
                )
                .await?;
            }
        } else if close_overlay {
            self.clear_interactive_overlay(attach_pid, true).await?;
        } else {
            self.refresh_interactive_overlay_if_active(attach_pid)
                .await?;
        }
        Ok(())
    }
}
