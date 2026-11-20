use rmux_proto::RmuxError;

use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use super::menu::PopupMenuAction;
use super::state::ClientOverlayState;
use crate::renderer::OverlayRect;

impl RequestHandler {
    pub(super) async fn apply_popup_menu_action(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        action: PopupMenuAction,
    ) -> Result<(), RmuxError> {
        if !self
            .overlay_action_is_current(attach_pid, identity)
            .await?
            .is_current()
        {
            return Ok(());
        }
        match action {
            PopupMenuAction::Close => {
                self.clear_interactive_overlay_for_optional_identity(attach_pid, identity, true)
                    .await?;
            }
            PopupMenuAction::Paste => {
                let bytes = {
                    let state = self.state.lock().await;
                    state
                        .buffers
                        .stack_head()
                        .and_then(|name| state.buffers.get(name))
                        .map(ToOwned::to_owned)
                        .unwrap_or_default()
                };
                let popup_write = {
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
                    if let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() {
                        popup.nested_menu = None;
                        Some(
                            popup
                                .job
                                .as_ref()
                                .and_then(|job| job.enqueue_write(&bytes).ok()),
                        )
                    } else {
                        None
                    }
                };
                if let Some(popup_write) = popup_write {
                    if let Some(receipt) = popup_write {
                        let _ = receipt.wait().await;
                    }
                    self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
                        .await?;
                }
            }
            PopupMenuAction::FillSpace | PopupMenuAction::Centre => {
                let popup_resize = {
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
                    let client_size = active.client_size;
                    if let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() {
                        popup.nested_menu = None;
                        match action {
                            PopupMenuAction::FillSpace => {
                                popup.rect = OverlayRect {
                                    x: 0,
                                    y: 0,
                                    width: client_size.cols.max(1),
                                    height: client_size.rows.max(1),
                                };
                                popup.preferred_width = popup.rect.width;
                                popup.preferred_height = popup.rect.height;
                            }
                            PopupMenuAction::Centre => {
                                popup.rect.x =
                                    client_size.cols.saturating_sub(popup.rect.width) / 2;
                                popup.rect.y =
                                    client_size.rows.saturating_sub(popup.rect.height) / 2;
                            }
                            _ => {}
                        }
                        let content_size = popup.content_size();
                        popup
                            .surface
                            .lock()
                            .expect("popup surface")
                            .resize(content_size);
                        Some(
                            popup
                                .job
                                .as_ref()
                                .and_then(|job| job.enqueue_resize(content_size).ok()),
                        )
                    } else {
                        None
                    }
                };
                if let Some(popup_resize) = popup_resize {
                    if let Some(receipt) = popup_resize {
                        let _ = receipt.wait().await;
                    }
                    self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
                        .await?;
                }
            }
            PopupMenuAction::HorizontalPane | PopupMenuAction::VerticalPane => {
                self.clear_interactive_overlay_for_optional_identity(attach_pid, identity, true)
                    .await?;
            }
        }
        Ok(())
    }
}
