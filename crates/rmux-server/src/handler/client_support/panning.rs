use rmux_proto::request::RefreshClientRequest;
use rmux_proto::{ErrorResponse, RefreshClientResponse, Response, RmuxError};

use super::super::{control_support::ManagedClient, RequestHandler};
use crate::client_viewport::PanAction;
use crate::handler_support::attached_client_required;

pub(super) fn pan_action(request: &RefreshClientRequest) -> Result<Option<PanAction>, RmuxError> {
    if !(request.clear_pan
        || request.pan_left
        || request.pan_right
        || request.pan_up
        || request.pan_down)
    {
        return Ok(None);
    }
    let n = request.adjustment.unwrap_or(1);
    if n == 0 || n > i32::MAX as u32 {
        return Err(RmuxError::Server(format!(
            "adjustment too {}",
            if n == 0 { "small" } else { "large" }
        )));
    }
    Ok(Some(if request.clear_pan {
        PanAction::Reset
    } else if request.pan_left {
        PanAction::Left(n)
    } else if request.pan_right {
        PanAction::Right(n)
    } else if request.pan_up {
        PanAction::Up(n)
    } else {
        PanAction::Down(n)
    }))
}

impl RequestHandler {
    pub(in crate::handler) async fn update_client_viewports(&self) {
        // Same lock order as size reconciliation and attached input mutation.
        let state = self.state.lock().await;
        let mut attaches = self.active_attach.lock().await;
        for active in attaches.by_pid.values_mut() {
            if active.closing.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            if let Some(session) = state.sessions.session(&active.session_name).filter(|s| {
                s.id() == active.session_id && s.window_at(s.active_window_index()).is_some()
            }) {
                active.viewport = active.pan.resolve(&state, session, active.client_size);
            }
        }
    }

    pub(in crate::handler) async fn attached_mouse_layout(
        &self,
        identity: super::super::attach_support::ActiveAttachIdentity,
        attached_count: usize,
    ) -> Option<crate::mouse::MouseLayout> {
        let state = self.state.lock().await;
        let mut attaches = self.active_attach.lock().await;
        let active = attaches
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|c| {
                identity.matches_active(c) && !c.closing.load(std::sync::atomic::Ordering::SeqCst)
            })?;
        Self::mouse_layout_for_active(&state, active, attached_count)
    }

    pub(in crate::handler) fn mouse_layout_for_active(
        state: &crate::pane_terminals::HandlerState,
        active: &mut super::super::attach_support::ActiveAttach,
        attached_count: usize,
    ) -> Option<crate::mouse::MouseLayout> {
        let session = state
            .sessions
            .session(&active.session_name)
            .filter(|s| s.id() == active.session_id)?;
        active.viewport = active.pan.resolve(state, session, active.client_size);
        let mut view = rmux_core::ManagedSessionPresentation::new(session);
        view.set_terminal_size(active.client_size);
        view.set_viewport_offset(
            active
                .viewport
                .bigger
                .then_some((active.viewport.x, active.viewport.y)),
        );
        crate::mouse::layout_for_view(state, view.view(), attached_count)
    }

    pub(super) async fn pan_client(&self, client: ManagedClient, action: PanAction) -> Response {
        let result = match client {
            ManagedClient::Attach { pid, attach_id } => self
                .pan_attached_client(pid, attach_id, action)
                .await
                .map(|()| pid),
            ManagedClient::Control(identity) => {
                // tmux accepts this route but has no terminal viewport to move.
                let controls = self.active_control.lock().await;
                controls
                    .by_pid
                    .get(&identity.requester_pid())
                    .filter(|c| {
                        c.id == identity.control_id()
                            && !c.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .map(|_| identity.requester_pid())
                    .ok_or_else(|| attached_client_required("refresh-client"))
            }
        };
        match result {
            Ok(pid) => Response::RefreshClient(RefreshClientResponse {
                target_client: pid.to_string(),
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    async fn pan_attached_client(
        &self,
        pid: u32,
        attach_id: u64,
        action: PanAction,
    ) -> Result<(), RmuxError> {
        let session_name = {
            let state = self.state.lock().await;
            let mut attaches = self.active_attach.lock().await;
            let active = attaches
                .by_pid
                .get_mut(&pid)
                .filter(|c| {
                    c.id == attach_id && !c.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            let session = state
                .sessions
                .session(&active.session_name)
                .filter(|s| s.id() == active.session_id)
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            let current = active.pan.resolve(&state, session, active.client_size);
            active.pan.apply(action, current);
            active.viewport = active.pan.resolve(&state, session, active.client_size);
            active.session_name.clone()
        };
        self.refresh_attached_client_for_identity(pid, attach_id, &session_name, "refresh-client")
            .await
    }
}

#[cfg(test)]
#[path = "../../handler_client/panning_tests.rs"]
mod tests;
