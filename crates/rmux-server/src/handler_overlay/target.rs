use rmux_proto::{PaneTarget, RmuxError, Target};

use super::super::RequestHandler;
use super::support::{find_session_name_by_id, find_window_target_by_id};
use crate::mouse::{AttachedMouseEvent, MouseLocation};

impl RequestHandler {
    pub(in crate::handler) async fn attached_mouse_target(
        &self,
        attach_pid: u32,
        event: &AttachedMouseEvent,
    ) -> Result<Option<Target>, RmuxError> {
        let session_name = self.attached_session_name(attach_pid).await?;
        self.overlay_target_from_mouse(session_name, Some(event))
            .await
    }

    pub(super) async fn resolve_overlay_client(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
        command_name: &str,
    ) -> Result<u32, RmuxError> {
        if let Some(target_client) = target_client {
            if target_client == "=" {
                return self
                    .resolve_attached_client_pid(requester_pid, command_name)
                    .await;
            }
            let pid = target_client.parse::<u32>().map_err(|_| {
                RmuxError::Server(format!("invalid {command_name} client '{target_client}'"))
            })?;
            let active_attach = self.active_attach.lock().await;
            if active_attach.by_pid.contains_key(&pid) {
                Ok(pid)
            } else {
                Err(RmuxError::Server(format!(
                    "{command_name} client {pid} is not attached"
                )))
            }
        } else {
            self.resolve_attached_client_pid(requester_pid, command_name)
                .await
        }
    }

    pub(super) async fn resolve_overlay_target(
        &self,
        attach_pid: u32,
        explicit_pane: Option<PaneTarget>,
        current_target: Option<Target>,
    ) -> Result<Target, RmuxError> {
        if let Some(target) = explicit_pane {
            return Ok(Target::Pane(target));
        }
        if let Some(target) = current_target {
            return Ok(target);
        }

        let session_name = self.attached_session_name(attach_pid).await?;
        let mouse_event = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .and_then(|active| active.mouse.current_event.clone())
        };
        if let Some(target) = self
            .overlay_target_from_mouse(session_name.clone(), mouse_event.as_ref())
            .await?
        {
            return Ok(target);
        }
        Ok(Target::Pane(self.attached_input_target(attach_pid).await?))
    }

    async fn overlay_target_from_mouse(
        &self,
        attached_session: rmux_proto::SessionName,
        event: Option<&AttachedMouseEvent>,
    ) -> Result<Option<Target>, RmuxError> {
        let Some(event) = event else {
            return Ok(None);
        };
        if let Some(target) = event.pane_target.clone() {
            return Ok(Some(Target::Pane(target)));
        }
        let state = self.state.lock().await;
        match event.location {
            MouseLocation::StatusLeft => Ok(Some(Target::Session(attached_session))),
            MouseLocation::Status | MouseLocation::StatusDefault | MouseLocation::StatusRight => {
                if let Some(window_id) = event.window_id
                    && let Some(target) =
                        find_window_target_by_id(&state, &attached_session, window_id)
                    {
                        return Ok(Some(Target::Window(target)));
                    }
                if let Some(session_name) = find_session_name_by_id(&state, event.session_id) {
                    return Ok(Some(Target::Session(session_name)));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}
