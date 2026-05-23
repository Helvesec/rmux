use rmux_core::LifecycleEvent;
use rmux_proto::{HookName, ScopeSelector, SessionName};

use crate::hook_runtime::PendingInlineHookFormat;

use super::super::{active_session_target, RequestHandler};

impl RequestHandler {
    pub(in crate::handler) async fn finish_new_session_lifecycle(
        &self,
        requester_pid: u32,
        session_name: &SessionName,
        detached: bool,
    ) {
        self.sync_session_silence_timers(session_name).await;
        let current_target = {
            let state = self.state.lock().await;
            active_session_target(&state.sessions, session_name)
        };
        self.queue_inline_hook(
            HookName::AfterNewSession,
            ScopeSelector::Session(session_name.clone()),
            current_target,
            PendingInlineHookFormat::AfterCommand,
        );
        let control_attached = !detached
            && self
                .prepare_created_session_control_attach(requester_pid, session_name)
                .await;
        self.emit(LifecycleEvent::SessionCreated {
            session_name: session_name.clone(),
        })
        .await;
        if control_attached {
            self.emit(LifecycleEvent::ClientSessionChanged {
                session_name: session_name.clone(),
                client_name: Some(requester_pid.to_string()),
            })
            .await;
        }
    }
}
