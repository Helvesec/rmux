use super::{LockAttachTarget, LockSessionPolicy, RequestHandler};
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::lifecycle_producer_tasks::LifecycleProducerLane;
use rmux_proto::{OptionName, SessionId};
use std::time::Duration;

#[derive(Debug)]
pub(in crate::handler) struct IdleLockTimer {
    task: tokio::task::JoinHandle<()>,
    session_id: SessionId,
    activity: rmux_core::SessionRecency,
    seconds: u32,
}
impl Drop for IdleLockTimer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn reset_attached_lock_timer(
        &self,
        identity: ActiveAttachIdentity,
    ) {
        let state = self.state.lock().await;
        let mut clients = self.active_attach.lock().await;
        let Some(active) = clients
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| identity.matches_active(active))
        else {
            return;
        };
        if active.suspended || active.closing.load(std::sync::atomic::Ordering::SeqCst) {
            active.idle_lock_timer = None;
            return;
        }
        let session_id = active.session_id;
        let Some(session) = state
            .sessions
            .session(&active.session_name)
            .filter(|session| session.id() == session_id)
        else {
            active.idle_lock_timer = None;
            return;
        };
        let activity = session.recency();
        let seconds = super::super::option_value_u32(
            &state.options,
            Some(&active.session_name),
            OptionName::LockAfterTime,
        );
        if seconds == 0 {
            active.idle_lock_timer = None;
            return;
        }
        if active.idle_lock_timer.as_ref().is_some_and(|timer| {
            timer.session_id == session_id
                && timer.activity == activity
                && timer.seconds == seconds
                && !timer.task.is_finished()
        }) {
            return;
        }
        active.idle_lock_timer = None;
        let Some(admission) = self
            .lifecycle_producers
            .try_register_in_lane(LifecycleProducerLane::Normal)
        else {
            return;
        };
        let handler = self.downgrade();
        let task = self.spawn_pre_admitted_lifecycle_producer_task_handle(
            "rmux-lock-timer",
            admission,
            async move {
                tokio::time::sleep(Duration::from_secs(u64::from(seconds))).await;
                if let Some(handler) = handler.upgrade() {
                    handler
                        .expire_idle_session_lock(identity, session_id, activity, seconds)
                        .await;
                }
            },
        );
        active.idle_lock_timer = Some(IdleLockTimer {
            task,
            session_id,
            activity,
            seconds,
        });
    }

    pub(in crate::handler) async fn reset_all_attached_lock_timers(&self) {
        let identities = self
            .active_attach
            .lock()
            .await
            .by_pid
            .iter()
            .map(|(pid, active)| active.identity(*pid))
            .collect::<Vec<_>>();
        for identity in identities {
            self.reset_attached_lock_timer(identity).await;
        }
    }

    async fn expire_idle_session_lock(
        &self,
        owner: ActiveAttachIdentity,
        session_id: SessionId,
        activity: rmux_core::SessionRecency,
        seconds: u32,
    ) {
        let targets = {
            let clients = self.active_attach.lock().await;
            if !clients
                .by_pid
                .get(&owner.attach_pid())
                .is_some_and(|active| {
                    owner.matches_active_lifetime(active, session_id) && !active.suspended
                })
            {
                return;
            }
            clients
                .by_pid
                .iter()
                .filter(|(_, active)| active.session_id == session_id)
                .map(|(pid, active)| LockAttachTarget {
                    identity: active.identity(*pid),
                    client_name: active.client_name.clone(),
                })
                .collect::<Vec<_>>()
        };
        for target in targets {
            let _ = self
                .lock_attached_client(
                    target,
                    LockSessionPolicy::Idle {
                        session_id,
                        activity,
                        seconds,
                    },
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_io::AttachControl;
    use rmux_proto::{
        NewSessionRequest, Request, ScopeSelector, SessionName, SetOptionMode, TerminalSize,
    };

    #[tokio::test]
    async fn idle_lock_timer_rejects_old_activity_then_locks_current_session() {
        let handler = RequestHandler::new();
        let name = SessionName::new("idle-lock-consumer").unwrap();
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: name.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        {
            let mut state = handler.state.lock().await;
            for (option, value) in [
                (OptionName::LockAfterTime, "1"),
                (OptionName::LockCommand, "idle-lock-probe"),
            ] {
                state
                    .options
                    .set(
                        ScopeSelector::Session(name.clone()),
                        option,
                        value.to_owned(),
                        SetOptionMode::Replace,
                    )
                    .unwrap();
            }
        }
        let pid = std::process::id();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let attach_id = handler.register_attach(pid, name.clone(), tx).await;
        let (session_id, old_activity) = {
            let mut state = handler.state.lock().await;
            let session = state.sessions.session(&name).unwrap();
            let captured = (session.id(), session.recency());
            state.sessions.touch_activity(&name).unwrap();
            captured
        };
        let identity = ActiveAttachIdentity::new(pid, attach_id, session_id);
        handler
            .expire_idle_session_lock(identity, session_id, old_activity, 1)
            .await;
        assert!(
            !handler
                .active_attach
                .lock()
                .await
                .by_pid
                .get(&pid)
                .unwrap()
                .suspended
        );
        handler.reset_all_attached_lock_timers().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(rx.recv().await, Some(AttachControl::LockShellCommand(_))) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            handler
                .active_attach
                .lock()
                .await
                .by_pid
                .get(&pid)
                .unwrap()
                .suspended
        );
        handler
            .state
            .lock()
            .await
            .options
            .set(
                ScopeSelector::Session(name),
                OptionName::LockAfterTime,
                "0".to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
        handler.handle_attached_unlock_for_identity(identity).await;
        assert!(handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&pid)
            .unwrap()
            .idle_lock_timer
            .is_none());
    }
}
