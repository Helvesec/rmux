use std::sync::{Arc, Condvar, Mutex as StdMutex};

use super::super::super::RequestHandler;

#[derive(Debug, Default)]
pub(in crate::handler) struct AttachedKeyTableTimerMutationPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    released: StdMutex<bool>,
    release: Condvar,
}

impl AttachedKeyTableTimerMutationPause {
    pub(in crate::handler) fn release(&self) {
        *self
            .released
            .lock()
            .expect("key table timer mutation pause lock") = true;
        self.release.notify_one();
    }

    fn wait(&self) {
        self.reached.notify_one();
        let mut released = self
            .released
            .lock()
            .expect("key table timer mutation pause lock");
        while !*released {
            released = self
                .release
                .wait(released)
                .expect("key table timer mutation pause lock");
        }
    }
}

#[derive(Debug, Default)]
pub(in crate::handler) struct AttachedKeyTableTimerAsyncPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

static TIMER_MUTATION_PAUSES: StdMutex<Vec<(usize, Arc<AttachedKeyTableTimerMutationPause>)>> =
    StdMutex::new(Vec::new());
static TIMER_EXPIRY_PAUSES: StdMutex<Vec<(usize, Arc<AttachedKeyTableTimerAsyncPause>)>> =
    StdMutex::new(Vec::new());
static TRANSITION_PAUSES: StdMutex<Vec<(usize, Arc<AttachedKeyTableTimerMutationPause>)>> =
    StdMutex::new(Vec::new());
static TIMER_REFRESH_PAUSES: StdMutex<Vec<(usize, Arc<AttachedKeyTableTimerAsyncPause>)>> =
    StdMutex::new(Vec::new());
static SWITCH_APPLY_PAUSES: StdMutex<Vec<(usize, u32, Arc<AttachedKeyTableTimerAsyncPause>)>> =
    StdMutex::new(Vec::new());

impl RequestHandler {
    pub(in crate::handler) fn install_attached_key_table_timer_mutation_pause(
        &self,
    ) -> Arc<AttachedKeyTableTimerMutationPause> {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = Arc::new(AttachedKeyTableTimerMutationPause::default());
        TIMER_MUTATION_PAUSES
            .lock()
            .expect("key table timer mutation pause registry lock")
            .push((handler_key, Arc::clone(&pause)));
        pause
    }

    pub(in crate::handler) fn install_attached_key_table_timer_refresh_pause(
        &self,
    ) -> Arc<AttachedKeyTableTimerAsyncPause> {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = Arc::new(AttachedKeyTableTimerAsyncPause::default());
        TIMER_REFRESH_PAUSES
            .lock()
            .expect("key table timer refresh pause registry lock")
            .push((handler_key, Arc::clone(&pause)));
        pause
    }

    pub(in crate::handler) fn install_attached_key_table_timer_expiry_pause(
        &self,
    ) -> Arc<AttachedKeyTableTimerAsyncPause> {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = Arc::new(AttachedKeyTableTimerAsyncPause::default());
        TIMER_EXPIRY_PAUSES
            .lock()
            .expect("key table timer expiry pause registry lock")
            .push((handler_key, Arc::clone(&pause)));
        pause
    }

    pub(in crate::handler) fn install_attached_key_table_switch_apply_pause(
        &self,
        attach_pid: u32,
    ) -> Arc<AttachedKeyTableTimerAsyncPause> {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = Arc::new(AttachedKeyTableTimerAsyncPause::default());
        SWITCH_APPLY_PAUSES
            .lock()
            .expect("key table switch apply pause registry lock")
            .push((handler_key, attach_pid, Arc::clone(&pause)));
        pause
    }

    pub(in crate::handler) fn install_attached_key_table_transition_pause(
        &self,
    ) -> Arc<AttachedKeyTableTimerMutationPause> {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = Arc::new(AttachedKeyTableTimerMutationPause::default());
        TRANSITION_PAUSES
            .lock()
            .expect("key table transition pause registry lock")
            .push((handler_key, Arc::clone(&pause)));
        pause
    }

    pub(super) fn pause_attached_key_table_timer_mutation(&self) {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        if let Some(pause) = take_timer_pause(&TIMER_MUTATION_PAUSES, handler_key) {
            pause.wait();
        }
    }

    pub(super) async fn pause_attached_key_table_timer_expiry(&self) {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        if let Some(pause) = take_timer_pause(&TIMER_EXPIRY_PAUSES, handler_key) {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    pub(in crate::handler) async fn pause_before_attached_key_table_switch_apply(
        &self,
        attach_pid: u32,
    ) {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        let pause = {
            let mut pauses = SWITCH_APPLY_PAUSES
                .lock()
                .expect("key table switch apply pause registry lock");
            pauses
                .iter()
                .position(|(key, pid, _)| *key == handler_key && *pid == attach_pid)
                .map(|position| pauses.swap_remove(position).2)
        };
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    pub(super) fn pause_attached_key_table_transition_commit(&self) {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        if let Some(pause) = take_timer_pause(&TRANSITION_PAUSES, handler_key) {
            pause.wait();
        }
    }

    pub(super) async fn pause_attached_key_table_timer_refresh(&self) {
        let handler_key = Arc::as_ptr(&self.lifecycle_producers) as usize;
        if let Some(pause) = take_timer_pause(&TIMER_REFRESH_PAUSES, handler_key) {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }
}

fn take_timer_pause<T>(
    pauses: &StdMutex<Vec<(usize, Arc<T>)>>,
    handler_key: usize,
) -> Option<Arc<T>> {
    let mut pauses = pauses.lock().expect("key table timer pause registry lock");
    pauses
        .iter()
        .position(|(key, _)| *key == handler_key)
        .map(|position| pauses.swap_remove(position).1)
}
