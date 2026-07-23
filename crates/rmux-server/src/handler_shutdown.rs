use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use rmux_proto::{OptionName, RmuxError};
use tokio::sync::watch;

use crate::diagnostic_log::{record_shutdown_queued, record_shutdown_request};
use crate::server_access::{AccessMode, ServerAccessAdmission};

use super::{
    DetachedRequesterAccess, DetachedRequesterAuthority, PendingShutdownReason, RequestHandler,
    RequesterOrigin,
};

const SHUTDOWN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleShutdownState {
    StillApplies,
    Stale,
    Unknown,
}

#[derive(Debug)]
pub(super) struct NormalRequestAdmission {
    open: std::sync::atomic::AtomicBool,
    active: AtomicUsize,
    active_drain: AtomicUsize,
    closing: watch::Sender<bool>,
}

impl NormalRequestAdmission {
    pub(super) fn new() -> Self {
        let (closing, _closing_rx) = watch::channel(false);
        Self {
            open: std::sync::atomic::AtomicBool::new(true),
            active: AtomicUsize::new(0),
            active_drain: AtomicUsize::new(0),
            closing,
        }
    }

    fn try_begin(self: &Arc<Self>, drain: bool) -> Option<NormalRequestGuard> {
        if !self.open.load(Ordering::SeqCst) {
            return None;
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        if drain {
            self.active_drain.fetch_add(1, Ordering::SeqCst);
        }
        let guard = NormalRequestGuard {
            admission: Arc::clone(self),
            drain,
        };
        if self.open.load(Ordering::SeqCst) {
            Some(guard)
        } else {
            // Pair the optimistic pre-check with a post-increment check. This
            // linearizes admission against `close`: either the request is
            // counted before close observes the barrier, or it is rejected.
            drop(guard);
            None
        }
    }
}

impl PendingShutdownReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ExitEmpty => "exit-empty",
            Self::KillServer => "kill-server",
            Self::SeamlessUpgradeIdle => "seamless-upgrade-idle",
        }
    }
}

impl RequestHandler {
    pub(crate) fn try_begin_normal_request(&self, drain: bool) -> Option<NormalRequestGuard> {
        self.normal_request_admission.try_begin(drain)
    }

    pub(crate) fn close_normal_request_admission(&self) {
        self.normal_request_admission
            .open
            .store(false, Ordering::SeqCst);
        self.normal_request_admission.closing.send_replace(true);
    }

    #[cfg(all(any(unix, windows), feature = "web"))]
    pub(crate) fn normal_request_shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.normal_request_admission.closing.subscribe()
    }

    pub(crate) fn normal_requests_quiesced(&self) -> bool {
        self.normal_request_admission.active.load(Ordering::SeqCst) == 0
    }

    pub(crate) fn normal_drain_requests_quiesced(&self) -> bool {
        self.normal_request_admission
            .active_drain
            .load(Ordering::SeqCst)
            == 0
    }

    pub(crate) fn begin_detached_connection(&self, connection_id: u64) -> DetachedConnectionGuard {
        self.active_detached_connections
            .lock()
            .expect("active detached connection mutex must not be poisoned")
            .insert(connection_id);
        DetachedConnectionGuard {
            connection_id,
            active_detached_connections: self.active_detached_connections.clone(),
        }
    }

    pub(crate) fn begin_detached_requester_access(
        &self,
        requester_pid: u32,
        admission: ServerAccessAdmission,
    ) -> DetachedRequesterAccessGuard {
        self.begin_detached_requester_authority(
            requester_pid,
            DetachedRequesterAuthority::Admission(admission),
        )
    }

    pub(crate) async fn begin_inherited_detached_requester_access(
        &self,
        requester_pid: u32,
    ) -> DetachedRequesterAccessGuard {
        let authority = self.requester_detached_authority(requester_pid).await;
        self.begin_detached_requester_authority(requester_pid, authority)
    }

    pub(in crate::handler) fn begin_requester_origin_access(
        &self,
        origin: &RequesterOrigin,
    ) -> DetachedRequesterAccessGuard {
        self.begin_detached_requester_authority(origin.requester_pid, origin.authority.clone())
    }

    pub(in crate::handler) async fn require_requester_origin_write(
        &self,
        origin: &RequesterOrigin,
    ) -> Result<DetachedRequesterAccessGuard, RmuxError> {
        let guard = self.begin_requester_origin_access(origin);
        let can_write = match &origin.authority {
            DetachedRequesterAuthority::Admission(admission) => self
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned")
                .revalidate_detached_admission(admission)
                .is_some_and(AccessMode::can_write),
            DetachedRequesterAuthority::Denied => false,
        };
        if !can_write {
            return Err(RmuxError::Server("client is read-only".to_owned()));
        }
        Ok(guard)
    }

    #[cfg(test)]
    pub(crate) fn begin_test_detached_requester_access(
        &self,
        requester_pid: u32,
        mode: AccessMode,
    ) -> DetachedRequesterAccessGuard {
        let admission = self
            .server_access
            .lock()
            .expect("server access mutex must not be poisoned")
            .owner_admission()
            .with_write_cap(mode.can_write());
        self.begin_detached_requester_access(requester_pid, admission)
    }

    fn begin_detached_requester_authority(
        &self,
        requester_pid: u32,
        authority: DetachedRequesterAuthority,
    ) -> DetachedRequesterAccessGuard {
        let mut access = self
            .active_detached_requester_access
            .lock()
            .expect("active detached requester access mutex must not be poisoned");
        let entry = access.entry(requester_pid).or_default();
        entry.scopes.push(authority.clone());
        DetachedRequesterAccessGuard {
            requester_pid,
            authority,
            active_detached_requester_access: self.active_detached_requester_access.clone(),
        }
    }

    pub(crate) fn begin_detached_request(&self) -> DetachedRequestGuard {
        self.active_detached_requests.fetch_add(1, Ordering::SeqCst);
        DetachedRequestGuard {
            active_detached_requests: self.active_detached_requests.clone(),
        }
    }

    pub(crate) fn begin_attach_forwarder(&self) -> AttachForwarderGuard {
        self.active_attach_forwarders.fetch_add(1, Ordering::SeqCst);
        AttachForwarderGuard {
            active_attach_forwarders: self.active_attach_forwarders.clone(),
        }
    }

    pub(crate) fn request_shutdown_if_pending(&self) -> bool {
        self.request_shutdown_if_pending_excluding_detached_connection(None)
    }

    pub(crate) fn request_shutdown_if_pending_excluding_detached_connection(
        &self,
        excluded_connection_id: Option<u64>,
    ) -> bool {
        if !self.shutdown_requested.load(Ordering::SeqCst) {
            return false;
        }
        let reason = *self
            .shutdown_reason
            .lock()
            .expect("shutdown reason mutex must not be poisoned");
        let force_shutdown = matches!(reason, Some(PendingShutdownReason::KillServer));
        if let Some(
            reason
            @ (PendingShutdownReason::ExitEmpty | PendingShutdownReason::SeamlessUpgradeIdle),
        ) = reason
        {
            match self.pending_idle_shutdown_state(reason, excluded_connection_id) {
                IdleShutdownState::StillApplies => {}
                IdleShutdownState::Stale => {
                    self.shutdown_requested.store(false, Ordering::SeqCst);
                    *self
                        .shutdown_reason
                        .lock()
                        .expect("shutdown reason mutex must not be poisoned") = None;
                    let stale_reason = format!("stale-{}-cancelled", reason.as_str());
                    record_shutdown_request(&stale_reason);
                    return false;
                }
                IdleShutdownState::Unknown => {
                    self.schedule_shutdown_retry(excluded_connection_id);
                    return false;
                }
            }
        }
        if !force_shutdown
            && !self
                .subscriptions
                .lock()
                .expect("subscription registry mutex must not be poisoned")
                .is_empty()
        {
            return false;
        }
        let retained_outputs_empty = force_shutdown || {
            let mut retained_outputs = self
                .retained_exited_outputs
                .lock()
                .expect("retained exited output mutex must not be poisoned");
            if retained_outputs.is_empty(std::time::Instant::now()) {
                true
            } else if matches!(
                reason,
                Some(PendingShutdownReason::ExitEmpty | PendingShutdownReason::SeamlessUpgradeIdle)
            ) {
                retained_outputs.clear();
                true
            } else {
                false
            }
        };
        if !retained_outputs_empty {
            return false;
        }
        if !self.shutdown_requested.swap(false, Ordering::SeqCst) {
            return false;
        }
        let reason = self
            .shutdown_reason
            .lock()
            .expect("shutdown reason mutex must not be poisoned")
            .take()
            .map(PendingShutdownReason::as_str)
            .unwrap_or("unknown");
        if let Some(handle) = self
            .shutdown_handle
            .lock()
            .expect("shutdown handle mutex must not be poisoned")
            .clone()
        {
            record_shutdown_request(reason);
            handle.request_shutdown();
        }
        true
    }

    fn schedule_shutdown_retry(&self, excluded_connection_id: Option<u64>) {
        let Some(runtime) = self
            .server_task_runtime()
            .or_else(|| tokio::runtime::Handle::try_current().ok())
        else {
            return;
        };
        let Ok(registration) = self.reserve_lifecycle_producer_task("rmux-shutdown-retry") else {
            return;
        };
        let Some(handoff) = registration.try_begin_mutation() else {
            return;
        };
        if self
            .shutdown_retry_scheduled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            drop(handoff);
            return;
        }

        let retry_handler = self.downgrade();
        let cleanup_handler = retry_handler.clone();
        runtime.spawn(async move {
            let retry = async move {
                tokio::time::sleep(SHUTDOWN_RETRY_DELAY).await;
                let Some(_retry_mutation) =
                    super::lifecycle_producer_tasks::begin_current_lifecycle_mutation()
                else {
                    // Yield back to the registered runner so lane cancellation
                    // performs the task-owned flag cleanup under its cleanup guard.
                    std::future::pending::<()>().await;
                    return;
                };
                let Some(handler) = retry_handler.upgrade() else {
                    return;
                };
                handler
                    .shutdown_retry_scheduled
                    .store(false, Ordering::SeqCst);
                let _ = handler.request_shutdown_if_pending_excluding_detached_connection(
                    excluded_connection_id,
                );
            };
            let cleanup = async move {
                if let Some(handler) = cleanup_handler.upgrade() {
                    handler
                        .shutdown_retry_scheduled
                        .store(false, Ordering::SeqCst);
                }
            };
            let _ = super::lifecycle_producer_tasks::run_registered_lifecycle_producer_with_cancellation_cleanup(
                registration,
                retry,
                cleanup,
            )
            .await;
        });
        drop(handoff);
    }

    pub(in crate::handler) fn queue_shutdown_request(&self, reason: PendingShutdownReason) {
        let mut pending_reason = self
            .shutdown_reason
            .lock()
            .expect("shutdown reason mutex must not be poisoned");
        if matches!(
            (*pending_reason, reason),
            (
                Some(PendingShutdownReason::KillServer),
                PendingShutdownReason::ExitEmpty
            )
        ) {
            return;
        }
        record_shutdown_queued(reason.as_str());
        *pending_reason = Some(reason);
        self.shutdown_requested.store(true, Ordering::SeqCst);
    }

    fn pending_idle_shutdown_state(
        &self,
        reason: PendingShutdownReason,
        excluded_connection_id: Option<u64>,
    ) -> IdleShutdownState {
        let Ok(state) = self.state.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        if !state.sessions.is_empty() {
            return IdleShutdownState::Stale;
        }
        if matches!(reason, PendingShutdownReason::ExitEmpty)
            && !matches!(
                state.options.resolve(None, OptionName::ExitEmpty),
                Some("on")
            )
        {
            return IdleShutdownState::Stale;
        }
        drop(state);

        let Ok(active_attach) = self.active_attach.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        if !active_attach.by_pid.is_empty() {
            return IdleShutdownState::Stale;
        }
        drop(active_attach);

        // Closing an attached client removes its registry entry before the
        // forwarder has drained the terminal exit frame to the client. Keep
        // exit-empty pending until that wire hand-off has completed.
        if self.active_attach_forwarders.load(Ordering::SeqCst) != 0 {
            return IdleShutdownState::Unknown;
        }

        if self.active_detached_requests.load(Ordering::SeqCst) != 0 {
            return IdleShutdownState::Unknown;
        }

        let Ok(active_detached_connections) = self.active_detached_connections.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        if active_detached_connections
            .iter()
            .any(|connection_id| Some(*connection_id) != excluded_connection_id)
        {
            return IdleShutdownState::Stale;
        }
        drop(active_detached_connections);

        let Ok(active_control) = self.active_control.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        if active_control.by_pid.is_empty() {
            return IdleShutdownState::StillApplies;
        }
        if active_control
            .by_pid
            .values()
            .any(|active| active.session_name.is_none() && !active.closing.load(Ordering::SeqCst))
        {
            return IdleShutdownState::Stale;
        }
        // A control client bound to the now-missing last session is removed by
        // the ordered lifecycle path. Treat that short hand-off as in flight:
        // cancelling exit-empty here would lose the only shutdown request
        // before finish_control removes the registration.
        IdleShutdownState::Unknown
    }
}

pub(crate) struct DetachedConnectionGuard {
    connection_id: u64,
    active_detached_connections: Arc<StdMutex<HashSet<u64>>>,
}

impl Drop for DetachedConnectionGuard {
    fn drop(&mut self) {
        self.active_detached_connections
            .lock()
            .expect("active detached connection mutex must not be poisoned")
            .remove(&self.connection_id);
    }
}

pub(crate) struct DetachedRequesterAccessGuard {
    requester_pid: u32,
    authority: DetachedRequesterAuthority,
    active_detached_requester_access: Arc<StdMutex<HashMap<u32, DetachedRequesterAccess>>>,
}

impl Drop for DetachedRequesterAccessGuard {
    fn drop(&mut self) {
        let mut access = self
            .active_detached_requester_access
            .lock()
            .expect("active detached requester access mutex must not be poisoned");
        let Some(entry) = access.get_mut(&self.requester_pid) else {
            return;
        };
        if let Some(position) = entry
            .scopes
            .iter()
            .position(|candidate| candidate == &self.authority)
        {
            entry.scopes.swap_remove(position);
        }
        if entry.is_empty() {
            access.remove(&self.requester_pid);
        }
    }
}

pub(crate) struct DetachedRequestGuard {
    active_detached_requests: Arc<AtomicUsize>,
}

#[derive(Debug)]
pub(crate) struct NormalRequestGuard {
    admission: Arc<NormalRequestAdmission>,
    drain: bool,
}

impl Drop for NormalRequestGuard {
    fn drop(&mut self) {
        if self.drain {
            self.admission.active_drain.fetch_sub(1, Ordering::SeqCst);
        }
        self.admission.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for DetachedRequestGuard {
    fn drop(&mut self) {
        self.active_detached_requests.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) struct AttachForwarderGuard {
    active_attach_forwarders: Arc<AtomicUsize>,
}

impl Drop for AttachForwarderGuard {
    fn drop(&mut self) {
        self.active_attach_forwarders.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ShutdownHandle;

    #[test]
    fn normal_request_close_linearizes_against_drain_admission() {
        let handler = RequestHandler::new();
        let admitted = handler
            .try_begin_normal_request(true)
            .expect("request is admitted before quiesce");

        handler.close_normal_request_admission();

        assert!(!handler.normal_requests_quiesced());
        assert!(!handler.normal_drain_requests_quiesced());
        assert!(
            handler.try_begin_normal_request(true).is_none(),
            "requests after the close linearization point are rejected"
        );

        drop(admitted);
        assert!(handler.normal_requests_quiesced());
        assert!(handler.normal_drain_requests_quiesced());
    }

    #[test]
    fn cancel_safe_requests_do_not_hold_the_drain_barrier() {
        let handler = RequestHandler::new();
        let admitted = handler
            .try_begin_normal_request(false)
            .expect("cancel-safe request is admitted before quiesce");
        handler.close_normal_request_admission();

        assert!(!handler.normal_requests_quiesced());
        assert!(handler.normal_drain_requests_quiesced());

        drop(admitted);
        assert!(handler.normal_requests_quiesced());
    }

    #[tokio::test]
    async fn idle_shutdown_retry_preserves_excluded_detached_connection() {
        let handler = RequestHandler::new();
        let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
        handler.install_shutdown_handle(shutdown_handle);

        let requester_connection_id = 7;
        let _requester_connection = handler.begin_detached_connection(requester_connection_id);
        handler.queue_shutdown_request(PendingShutdownReason::SeamlessUpgradeIdle);

        let active_connections = handler
            .active_detached_connections
            .lock()
            .expect("active detached connection mutex must not be poisoned");
        assert!(
            !handler.request_shutdown_if_pending_excluding_detached_connection(Some(
                requester_connection_id
            ))
        );
        drop(active_connections);

        tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx)
            .await
            .expect("retry should preserve requester exclusion and request shutdown")
            .expect("shutdown receiver should complete cleanly");
    }

    #[tokio::test]
    async fn idle_shutdown_retries_after_in_flight_detached_request() {
        let handler = RequestHandler::new();
        let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
        handler.install_shutdown_handle(shutdown_handle);
        let _request = handler.begin_detached_request();

        handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
        assert!(
            !handler.request_shutdown_if_pending(),
            "in-flight detached requests should defer, not cancel, exit-empty shutdown"
        );
        drop(_request);

        tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx)
            .await
            .expect("retry should request shutdown after detached request finishes")
            .expect("shutdown receiver should complete cleanly");
    }

    #[tokio::test]
    async fn idle_shutdown_retries_after_attach_forwarder_drain() {
        let handler = RequestHandler::new();
        let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
        handler.install_shutdown_handle(shutdown_handle);
        let forwarder = handler.begin_attach_forwarder();

        handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
        assert!(
            !handler.request_shutdown_if_pending(),
            "an attached wire drain should defer, not cancel, exit-empty shutdown"
        );
        drop(forwarder);

        tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx)
            .await
            .expect("retry should request shutdown after the attach forwarder drains")
            .expect("shutdown receiver should complete cleanly");
    }

    #[tokio::test]
    async fn lifecycle_close_cancels_pending_shutdown_retry_and_cleans_its_flag() {
        let handler = RequestHandler::new();
        let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
        handler.install_shutdown_handle(shutdown_handle);
        let state = handler.state.lock().await;

        handler.queue_shutdown_request(PendingShutdownReason::ExitEmpty);
        assert!(
            !handler.request_shutdown_if_pending(),
            "the held state lock forces the retry path"
        );
        assert!(handler.shutdown_retry_scheduled.load(Ordering::SeqCst));

        handler.close_normal_and_drain_lifecycle_producers().await;
        assert!(
            !handler.shutdown_retry_scheduled.load(Ordering::SeqCst),
            "cancellation cleanup owns the scheduled flag"
        );
        drop(state);

        assert!(
            tokio::time::timeout(SHUTDOWN_RETRY_DELAY * 2, shutdown_rx)
                .await
                .is_err(),
            "a cancelled retry cannot request shutdown after the lane is sealed"
        );
    }
}
