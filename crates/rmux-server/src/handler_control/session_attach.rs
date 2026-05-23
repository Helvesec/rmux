use rmux_proto::SessionName;

use crate::handler::RequestHandler;

impl RequestHandler {
    pub(in crate::handler) async fn prepare_created_session_control_attach(
        &self,
        requester_pid: u32,
        session_name: &SessionName,
    ) -> bool {
        if !self.is_control_client(requester_pid).await {
            return false;
        }

        let window_id = {
            let mut state = self.state.lock().await;
            let Some(session) = state.sessions.session_mut(session_name) else {
                return false;
            };
            session.touch_attached();
            session
                .window_at(session.active_window_index())
                .map(|window| window.id().as_u32())
        };

        if let Some(window_id) = window_id {
            self.send_control_notification_to(requester_pid, format!("%window-add @{window_id}"))
                .await;
        }

        self.set_control_session(requester_pid, Some(session_name.clone()))
            .await
            .is_ok()
    }

    pub(in crate::handler) async fn attach_control_to_existing_session(
        &self,
        requester_pid: u32,
        session_name: &SessionName,
    ) -> bool {
        if !self.is_control_client(requester_pid).await {
            return false;
        }
        if self
            .control_session_name(requester_pid)
            .await
            .as_ref()
            .is_some_and(|current| current == session_name)
        {
            return false;
        }

        {
            let mut state = self.state.lock().await;
            if let Some(session) = state.sessions.session_mut(session_name) {
                session.touch_attached();
            }
        }

        self.set_control_session(requester_pid, Some(session_name.clone()))
            .await
            .is_ok()
    }
}
