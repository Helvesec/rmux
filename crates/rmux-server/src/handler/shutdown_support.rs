use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use rmux_ipc::PeerIdentity;
use rmux_proto::RmuxError;
use tokio::sync::watch;

use crate::diagnostic_log::{record_shutdown_queued, record_shutdown_request};
use crate::pane_terminals::PaneTerminalShutdown;
use crate::server_access::{AccessMode, ServerAccessAdmission};

use super::{
    DetachedRequesterAccess, DetachedRequesterAuthority, DetachedRequesterScope, RequestHandler,
    RequesterOrigin,
};

mod retry;
mod shutdown_idle_policy;
pub(in crate::handler) use retry::ShutdownRetryState;

const SHUTDOWN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

impl RequestHandler {
    pub(crate) async fn shutdown_terminal_processes(&self) -> std::io::Result<()> {
        let shutdown: PaneTerminalShutdown = self
            .state
            .lock()
            .await
            .take_terminal_processes_for_shutdown();
        shutdown.finish().await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum PendingShutdownReason {
    #[cfg(unix)]
    AbandonedStartup,
    ExitEmpty,
    ExitUnattached,
    KillServer,
    SeamlessUpgradeIdle,
}

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
    fn is_idle(self) -> bool {
        !matches!(self, Self::KillServer)
    }

    fn as_str(self) -> &'static str {
        match self {
            #[cfg(unix)]
            Self::AbandonedStartup => "abandoned-startup",
            Self::ExitEmpty => "exit-empty",
            Self::ExitUnattached => "exit-unattached",
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

    /// A requester scope carrying an admission but no authenticated peer.
    ///
    /// Every production connection now opens its scope through
    /// [`Self::begin_authenticated_peer_access`], which carries the peer the OS
    /// authenticated (issue #182); this remains only for the tests that drive a
    /// requester scope without a connection behind it.
    #[cfg(test)]
    pub(crate) fn begin_detached_requester_access(
        &self,
        requester_pid: u32,
        admission: ServerAccessAdmission,
    ) -> DetachedRequesterAccessGuard {
        self.begin_detached_requester_scope(
            requester_pid,
            DetachedRequesterScope::new(DetachedRequesterAuthority::Admission(admission), None),
        )
    }

    /// Opens the scope a connection loop runs one dispatched request under,
    /// carrying the local peer the OS authenticated for that connection.
    ///
    /// An attach frame is rendered before the listener registers the client,
    /// so this is how that render reaches the same `#{client_uid}` /
    /// `#{client_user}` the registration then stores (issue #182).
    pub(crate) fn begin_authenticated_peer_access(
        &self,
        peer: &PeerIdentity,
        admission: ServerAccessAdmission,
    ) -> DetachedRequesterAccessGuard {
        self.begin_detached_requester_scope(
            peer.pid,
            DetachedRequesterScope::new(
                DetachedRequesterAuthority::Admission(admission),
                Some(peer.clone()),
            ),
        )
    }

    pub(crate) async fn begin_inherited_detached_requester_access(
        &self,
        requester_pid: u32,
    ) -> DetachedRequesterAccessGuard {
        let authority = self.requester_detached_authority(requester_pid).await;
        // A nested scope inherits the peer with the authority, so running a
        // hook or a shell under an authenticated connection does not turn its
        // own requester identity ambiguous.
        let peer = self.authenticated_requester_peer(requester_pid);
        self.begin_detached_requester_scope(
            requester_pid,
            DetachedRequesterScope::new(authority, peer),
        )
    }

    pub(in crate::handler) fn begin_requester_origin_access(
        &self,
        origin: &RequesterOrigin,
    ) -> DetachedRequesterAccessGuard {
        self.begin_detached_requester_scope(
            origin.requester_pid,
            DetachedRequesterScope::new(origin.authority.clone(), None),
        )
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

    fn begin_detached_requester_scope(
        &self,
        requester_pid: u32,
        scope: DetachedRequesterScope,
    ) -> DetachedRequesterAccessGuard {
        let mut access = self
            .active_detached_requester_access
            .lock()
            .expect("active detached requester access mutex must not be poisoned");
        let entry = access.entry(requester_pid).or_default();
        entry.scopes.push(scope.clone());
        DetachedRequesterAccessGuard {
            requester_pid,
            scope,
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
        // A Unix reply does not close its persistent client. Only an explicit
        // idle-upgrade request may exclude that connection, including on retry.
        #[cfg(unix)]
        let excluded_connection_id = if matches!(
            reason,
            Some(
                PendingShutdownReason::AbandonedStartup
                    | PendingShutdownReason::ExitEmpty
                    | PendingShutdownReason::ExitUnattached
            )
        ) {
            None
        } else {
            excluded_connection_id
        };
        let mut idle_exclusion_evaluation = None;
        if let Some(reason) = reason.filter(|reason| reason.is_idle()) {
            let effective_excluded_connection_id = self
                .shutdown_retry_state
                .lock()
                .expect("shutdown retry state mutex must not be poisoned")
                .effective_exclusion(excluded_connection_id);
            idle_exclusion_evaluation =
                Some((excluded_connection_id, effective_excluded_connection_id));
            match self.pending_idle_shutdown_state(reason, effective_excluded_connection_id) {
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
                    self.schedule_shutdown_retry(effective_excluded_connection_id);
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
            } else if reason.is_some_and(PendingShutdownReason::is_idle) {
                retained_outputs.clear();
                true
            } else {
                false
            }
        };
        if !retained_outputs_empty {
            return false;
        }
        let retry_state_guard =
            if let Some((requested_exclusion, evaluated_exclusion)) = idle_exclusion_evaluation {
                let mut retry_state = self
                    .shutdown_retry_state
                    .lock()
                    .expect("shutdown retry state mutex must not be poisoned");
                let current_exclusion = retry_state.effective_exclusion(requested_exclusion);
                if current_exclusion != evaluated_exclusion {
                    let tightened_exclusion =
                        retry::tighten_exclusions(current_exclusion, evaluated_exclusion);
                    drop(retry_state);
                    self.schedule_shutdown_retry(tightened_exclusion);
                    return false;
                }
                Some(retry_state)
            } else {
                None
            };
        if !self.shutdown_requested.swap(false, Ordering::SeqCst) {
            return false;
        }
        let reason = self
            .shutdown_reason
            .lock()
            .expect("shutdown reason mutex must not be poisoned")
            .take();
        if let Some(handle) = self
            .shutdown_handle
            .lock()
            .expect("shutdown handle mutex must not be poisoned")
            .clone()
        {
            record_shutdown_request(reason.map_or("unknown", PendingShutdownReason::as_str));
            if reason.is_some_and(PendingShutdownReason::releases_endpoint_early) {
                handle.request_idle_shutdown();
            } else {
                handle.request_shutdown();
            }
        }
        drop(retry_state_guard);
        true
    }

    fn schedule_shutdown_retry(&self, excluded_connection_id: Option<u64>) {
        if self
            .shutdown_retry_state
            .lock()
            .expect("shutdown retry state mutex must not be poisoned")
            .tighten_if_scheduled(excluded_connection_id)
        {
            return;
        }
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
        let Some(retry_token) = self
            .shutdown_retry_state
            .lock()
            .expect("shutdown retry state mutex must not be poisoned")
            .schedule_or_tighten(excluded_connection_id)
        else {
            drop(handoff);
            return;
        };

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
                let Some(excluded_connection_id) = handler
                    .shutdown_retry_state
                    .lock()
                    .expect("shutdown retry state mutex must not be poisoned")
                    .take(retry_token)
                else {
                    return;
                };
                let _ = handler.request_shutdown_if_pending_excluding_detached_connection(
                    excluded_connection_id,
                );
            };
            let cleanup = async move {
                if let Some(handler) = cleanup_handler.upgrade() {
                    handler
                        .shutdown_retry_state
                        .lock()
                        .expect("shutdown retry state mutex must not be poisoned")
                        .cancel(retry_token);
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
        // Startup abandonment must not replace an ordinary lifecycle decision
        // with its more restrictive web policy, nor weaken a forced shutdown.
        #[cfg(unix)]
        if reason == PendingShutdownReason::AbandonedStartup && pending_reason.is_some() {
            return;
        }
        if matches!(
            (*pending_reason, reason),
            (
                Some(PendingShutdownReason::KillServer),
                PendingShutdownReason::ExitEmpty | PendingShutdownReason::ExitUnattached
            )
        ) {
            return;
        }
        record_shutdown_queued(reason.as_str());
        *pending_reason = Some(reason);
        self.shutdown_requested.store(true, Ordering::SeqCst);
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
    scope: DetachedRequesterScope,
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
            .position(|candidate| candidate == &self.scope)
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
#[path = "../handler_shutdown/tests.rs"]
mod tests;
