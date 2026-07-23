//! Lifecycle ownership for delayed attached-mouse click classification.

use std::io;
use std::time::Instant;

use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::lifecycle_producer_tasks::{
    begin_current_lifecycle_mutation, current_lifecycle_producer_can_continue,
    LifecycleProducerLane,
};
use crate::handler::RequestHandler;
use crate::mouse::{layout_for_session, MouseClickTimerToken};

const ATTACHED_MOUSE_CLICK_TIMER_TASK: &str = "rmux-attached-mouse-click-timer";
const SESSION_NAME_REVALIDATION_ATTEMPTS: usize = 4;

impl RequestHandler {
    pub(super) fn schedule_attached_mouse_click_timer(
        &self,
        identity: ActiveAttachIdentity,
        token: MouseClickTimerToken,
    ) {
        let Some(registration) = self
            .lifecycle_producers
            .try_register_in_lane(LifecycleProducerLane::Normal)
        else {
            return;
        };
        let handler = self.clone();
        drop(self.spawn_pre_admitted_lifecycle_producer_task_handle(
            ATTACHED_MOUSE_CLICK_TIMER_TASK,
            registration,
            async move {
                tokio::time::sleep_until(tokio::time::Instant::from_std(token.deadline())).await;
                let _ = handler
                    .dispatch_expired_attached_mouse_click(identity, token)
                    .await;
            },
        ));
    }

    async fn dispatch_expired_attached_mouse_click(
        &self,
        identity: ActiveAttachIdentity,
        token: MouseClickTimerToken,
    ) -> io::Result<()> {
        let attach_pid = identity.attach_pid();
        for attempt in 0..SESSION_NAME_REVALIDATION_ATTEMPTS {
            let (session_name, session_id) = {
                let active_attach = self.active_attach.lock().await;
                let Some(active) = active_attach.by_pid.get(&attach_pid).filter(|active| {
                    identity.matches_active(active)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                        && active.mouse.click_timer_matches(token)
                }) else {
                    return Ok(());
                };
                (active.session_name.clone(), active.session_id)
            };
            let attached_count = self
                .attached_count_for_session_identity(&session_name, session_id)
                .await;

            // Acquire every potentially contended lock before entering the
            // bounded lifecycle mutation. Closure then linearizes with only
            // identity revalidation and the in-memory click-state transition.
            let state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(mutation) = begin_current_lifecycle_mutation() else {
                return Ok(());
            };
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(());
            };
            if !identity.matches_active(active)
                || active.closing.load(std::sync::atomic::Ordering::SeqCst)
                || !active.mouse.click_timer_matches(token)
            {
                return Ok(());
            }

            let current_session_matches = active.session_id == identity.session_id();
            let captured_name_is_current = active.session_name == session_name;
            if current_session_matches
                && !captured_name_is_current
                && attempt + 1 < SESSION_NAME_REVALIDATION_ATTEMPTS
            {
                drop(mutation);
                drop(active_attach);
                drop(state);
                continue;
            }

            let session_is_current = current_session_matches
                && captured_name_is_current
                && state
                    .sessions
                    .session(&session_name)
                    .is_some_and(|session| session.id() == session_id);
            let classified = if session_is_current {
                if let Some(layout) = layout_for_session(&state, &session_name, attached_count) {
                    active.mouse.expire_click_timer(Instant::now(), &layout)
                } else {
                    active.mouse.clear_click_timer_if_current(token);
                    None
                }
            } else {
                active.mouse.clear_click_timer_if_current(token);
                None
            };
            let mutated = !active.mouse.click_timer_matches(token);

            #[cfg(test)]
            let timer_pause = if mutated {
                self.take_attached_mouse_timer_pause()
            } else {
                None
            };
            #[cfg(test)]
            if let Some(pause) = &timer_pause {
                pause.mutation_reached.wait();
                pause.mutation_release.wait();
            }

            drop(mutation);
            drop(active_attach);
            drop(state);

            if !mutated {
                self.schedule_attached_mouse_click_timer(identity, token);
                return Ok(());
            }

            #[cfg(test)]
            if let Some(pause) = &timer_pause {
                pause.dispatch_reached.notify_one();
                pause.dispatch_release.notified().await;
            }

            let Some(classified) = classified else {
                #[cfg(test)]
                if let Some(pause) = timer_pause {
                    pause.task_completed.notify_one();
                }
                return Ok(());
            };
            if !current_lifecycle_producer_can_continue() {
                #[cfg(test)]
                if let Some(pause) = timer_pause {
                    pause.task_completed.notify_one();
                }
                return Ok(());
            }
            // Return to the registered runner after the local mutation. A lane
            // closure that raced the timer can now cancel it without waiting
            // on command dispatch, refreshes, or pane I/O.
            tokio::task::yield_now().await;
            if !current_lifecycle_producer_can_continue() {
                #[cfg(test)]
                if let Some(pause) = timer_pause {
                    pause.task_completed.notify_one();
                }
                return Ok(());
            }

            let result = self
                .dispatch_attached_mouse_classified(identity, &session_name, session_id, classified)
                .await;
            #[cfg(test)]
            if let Some(pause) = timer_pause {
                pause.task_completed.notify_one();
            }
            return result;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::handler) async fn dispatch_expired_attached_mouse_click_for_test(
        &self,
        identity: ActiveAttachIdentity,
        token: MouseClickTimerToken,
    ) -> io::Result<()> {
        self.dispatch_expired_attached_mouse_click(identity, token)
            .await
    }
}

#[cfg(test)]
mod test_support {
    use std::sync::{Arc, Barrier, Mutex};

    use tokio::sync::Notify;

    use super::RequestHandler;

    #[derive(Debug)]
    pub(in crate::handler) struct AttachedMouseTimerPause {
        pub(in crate::handler) mutation_reached: Barrier,
        pub(in crate::handler) mutation_release: Barrier,
        pub(in crate::handler) dispatch_reached: Notify,
        pub(in crate::handler) dispatch_release: Notify,
        pub(in crate::handler) task_completed: Notify,
    }

    impl Default for AttachedMouseTimerPause {
        fn default() -> Self {
            Self {
                mutation_reached: Barrier::new(2),
                mutation_release: Barrier::new(2),
                dispatch_reached: Notify::new(),
                dispatch_release: Notify::new(),
                task_completed: Notify::new(),
            }
        }
    }

    static PAUSES: Mutex<Vec<(usize, Arc<AttachedMouseTimerPause>)>> = Mutex::new(Vec::new());

    impl RequestHandler {
        pub(in crate::handler) fn install_attached_mouse_timer_pause(
            &self,
        ) -> Arc<AttachedMouseTimerPause> {
            let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
            let pause = Arc::new(AttachedMouseTimerPause::default());
            let mut pauses = PAUSES
                .lock()
                .expect("attached mouse timer pause registry lock");
            pauses.retain(|(key, _)| *key != handler_key);
            pauses.push((handler_key, Arc::clone(&pause)));
            pause
        }

        pub(super) fn take_attached_mouse_timer_pause(
            &self,
        ) -> Option<Arc<AttachedMouseTimerPause>> {
            let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
            let mut pauses = PAUSES
                .lock()
                .expect("attached mouse timer pause registry lock");
            pauses
                .iter()
                .position(|(key, _)| *key == handler_key)
                .map(|position| pauses.swap_remove(position).1)
        }
    }
}
