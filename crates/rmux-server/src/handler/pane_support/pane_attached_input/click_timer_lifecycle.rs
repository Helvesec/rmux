//! Lifecycle ownership for delayed attached-mouse click classification.

use std::io;
use std::time::Instant;

use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::lifecycle_producer_tasks::{
    begin_current_lifecycle_mutation, current_lifecycle_producer_can_continue,
    LifecycleProducerLane,
};
use crate::handler::RequestHandler;
use crate::mouse::MouseClickTimerToken;

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
                handler
                    .sleep_until_attached_mouse_click_deadline(token)
                    .await;
                let _ = handler
                    .dispatch_expired_attached_mouse_click(identity, token)
                    .await;
            },
        ));
    }

    async fn sleep_until_attached_mouse_click_deadline(&self, token: MouseClickTimerToken) {
        // A test click clock stands in for real time: the click deadline
        // passes when the test opens the expiry gate, never on its own.
        #[cfg(test)]
        if let Some(clock) = self.installed_attached_mouse_click_clock() {
            clock.wait_until_expiry_allowed().await;
            return;
        }
        tokio::time::sleep_until(tokio::time::Instant::from_std(token.deadline())).await;
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
                if let Some(layout) = Self::mouse_layout_for_active(&state, active, attached_count)
                {
                    let now = Instant::now();
                    // Under a test click clock the gated timer fires exactly
                    // at its deadline.
                    #[cfg(test)]
                    let now = self.attached_mouse_click_expiry_instant_for_test(token, now);
                    active.mouse.expire_click_timer(now, &layout)
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
                self.take_attached_mouse_timer_rendezvous()
            } else {
                None
            };
            // The pause is asynchronous: a test that misses this rendezvous
            // fails on its bounded wait and its runtime can still shut down.
            #[cfg(test)]
            if let Some(pause) = &timer_pause {
                pause.mutation_reached.notify_one();
                pause.mutation_release.notified().await;
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
pub(in crate::handler) use test_support::AttachedMouseTimerPause;

#[cfg(test)]
mod test_support {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use tokio::sync::Notify;

    use super::RequestHandler;
    use crate::mouse::MouseClickTimerToken;

    /// Test clock for one handler's attached mouse clicks. Every click is
    /// classified at the instant the clock was installed, so load can never
    /// split a double click, and a click timer expires at its own deadline
    /// only once the test calls `allow_expiry()`. With a rendezvous armed, the
    /// first timer that mutates click state also stops at each `*_reached`
    /// point until the test releases it.
    #[derive(Debug)]
    pub(in crate::handler) struct AttachedMouseTimerPause {
        click_instant: Instant,
        rendezvous_armed: AtomicBool,
        expiry_allowed: AtomicBool,
        expiry_gate: Notify,
        pub(in crate::handler) mutation_reached: Notify,
        pub(in crate::handler) mutation_release: Notify,
        pub(in crate::handler) dispatch_reached: Notify,
        pub(in crate::handler) dispatch_release: Notify,
        pub(in crate::handler) task_completed: Notify,
    }

    impl AttachedMouseTimerPause {
        fn new(rendezvous: bool) -> Self {
            Self {
                click_instant: Instant::now(),
                rendezvous_armed: AtomicBool::new(rendezvous),
                expiry_allowed: AtomicBool::new(false),
                expiry_gate: Notify::new(),
                mutation_reached: Notify::new(),
                mutation_release: Notify::new(),
                dispatch_reached: Notify::new(),
                dispatch_release: Notify::new(),
                task_completed: Notify::new(),
            }
        }

        /// Lets the handler's click timers reach their deadlines. Until then
        /// they wait before taking any lock, so the test can finish its own
        /// requests however long load delays them.
        pub(in crate::handler) fn allow_expiry(&self) {
            self.expiry_allowed.store(true, Ordering::SeqCst);
            self.expiry_gate.notify_waiters();
        }

        pub(super) async fn wait_until_expiry_allowed(&self) {
            loop {
                let allowed = self.expiry_gate.notified();
                tokio::pin!(allowed);
                allowed.as_mut().enable();
                if self.expiry_allowed.load(Ordering::SeqCst) {
                    return;
                }
                allowed.await;
            }
        }
    }

    impl RequestHandler {
        /// Installs a click clock whose first mutating timer also stops at the
        /// rendezvous points.
        pub(in crate::handler) fn install_attached_mouse_timer_pause(
            &self,
        ) -> Arc<AttachedMouseTimerPause> {
            self.install_attached_mouse_timer_clock(true)
        }

        /// Installs a click clock without a rendezvous.
        pub(in crate::handler) fn install_attached_mouse_click_clock(
            &self,
        ) -> Arc<AttachedMouseTimerPause> {
            self.install_attached_mouse_timer_clock(false)
        }

        fn install_attached_mouse_timer_clock(
            &self,
            rendezvous: bool,
        ) -> Arc<AttachedMouseTimerPause> {
            let clock = Arc::new(AttachedMouseTimerPause::new(rendezvous));
            *self
                .attached_mouse_timer_pause
                .lock()
                .expect("attached mouse timer pause lock") = Some(Arc::clone(&clock));
            clock
        }

        pub(super) fn installed_attached_mouse_click_clock(
            &self,
        ) -> Option<Arc<AttachedMouseTimerPause>> {
            self.attached_mouse_timer_pause
                .lock()
                .expect("attached mouse timer pause lock")
                .clone()
        }

        pub(in crate::handler) fn attached_mouse_click_instant_for_test(
            &self,
            now: Instant,
        ) -> Instant {
            self.installed_attached_mouse_click_clock()
                .map_or(now, |clock| clock.click_instant)
        }

        pub(super) fn attached_mouse_click_expiry_instant_for_test(
            &self,
            token: MouseClickTimerToken,
            now: Instant,
        ) -> Instant {
            self.installed_attached_mouse_click_clock()
                .map_or(now, |_| token.deadline())
        }

        pub(super) fn take_attached_mouse_timer_rendezvous(
            &self,
        ) -> Option<Arc<AttachedMouseTimerPause>> {
            self.installed_attached_mouse_click_clock()
                .filter(|pause| pause.rendezvous_armed.swap(false, Ordering::SeqCst))
        }
    }
}
