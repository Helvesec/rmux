use rmux_core::LifecycleEvent;
use rmux_proto::{HookName, SessionId, SessionName};

use crate::hook_runtime::PendingInlineHookFormat;

use super::super::RequestHandler;

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct CreatedSessionControlAttachPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static CREATED_SESSION_CONTROL_ATTACH_PAUSES: std::sync::Mutex<
    Vec<(
        SessionName,
        std::sync::Arc<CreatedSessionControlAttachPause>,
    )>,
> = std::sync::Mutex::new(Vec::new());

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_created_session_control_attach_pause(
        &self,
        session_name: SessionName,
    ) -> std::sync::Arc<CreatedSessionControlAttachPause> {
        let pause = std::sync::Arc::new(CreatedSessionControlAttachPause::default());
        let mut pauses = CREATED_SESSION_CONTROL_ATTACH_PAUSES
            .lock()
            .expect("created-session control attach pause lock");
        pauses.retain(|(paused_session, _)| paused_session != &session_name);
        pauses.push((session_name, pause.clone()));
        pause
    }

    #[cfg(test)]
    pub(in crate::handler) async fn pause_before_created_session_control_attach(
        &self,
        session_name: &SessionName,
    ) {
        let pause = CREATED_SESSION_CONTROL_ATTACH_PAUSES
            .lock()
            .expect("created-session control attach pause lock")
            .iter()
            .find(|(paused_session, _)| paused_session == session_name)
            .map(|(_, pause)| pause.clone());
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
        let mut pauses = CREATED_SESSION_CONTROL_ATTACH_PAUSES
            .lock()
            .expect("created-session control attach pause lock");
        pauses.retain(|(paused_session, current)| {
            paused_session != session_name || !std::sync::Arc::ptr_eq(current, &pause)
        });
    }

    #[cfg(not(test))]
    pub(in crate::handler) async fn pause_before_created_session_control_attach(
        &self,
        _session_name: &SessionName,
    ) {
    }

    pub(in crate::handler) async fn finish_new_session_lifecycle(
        &self,
        requester_pid: u32,
        session_name: &SessionName,
        session_id: SessionId,
        template_session: Option<&SessionName>,
        detached: bool,
    ) {
        self.sync_new_session_silence_timers(session_name, session_id, template_session)
            .await;
        self.queue_exact_session_inline_hook(
            HookName::AfterNewSession,
            session_name.clone(),
            session_id,
            None,
            PendingInlineHookFormat::AfterCommand,
        );
        let control_attached = !detached
            && self
                .prepare_created_session_control_attach(requester_pid, session_name, session_id)
                .await;
        self.emit_for_session_identity(
            LifecycleEvent::SessionCreated {
                session_name: session_name.clone(),
            },
            session_name,
            session_id,
        )
        .await;
        if control_attached {
            self.emit_for_session_identity(
                LifecycleEvent::ClientSessionChanged {
                    session_name: session_name.clone(),
                    client_name: Some(requester_pid.to_string()),
                },
                session_name,
                session_id,
            )
            .await;
        }
    }
}
