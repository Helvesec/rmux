use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use crate::client_activity::ClientActivityTimestamp;
use rmux_core::events::{PaneSnapshotCoalescerRegistry, SubscriptionLimits};
use rmux_ipc::PeerIdentity;
use rmux_proto::{RmuxError, TerminalSize, WindowTarget};
use tokio::sync::{broadcast, Mutex, Notify};

use crate::daemon::ShutdownHandle;
mod alert_support;
pub(crate) mod attach_support;
mod background_tasks;
mod buffer_support;
mod client_environment_support;
mod client_runtime_support;
mod client_support;
pub(crate) use client_support::attach_resize_publication::AttachResizePublication;
mod clipboard_query_support;
#[cfg(test)]
#[path = "handler_clipboard_query_test_pause.rs"]
mod clipboard_query_test_pause;
mod clock_mode_support;
mod config_support;
mod construction;
mod control_support;
mod copy_mode_support;
mod daemon_support;
mod dispatch_support;
mod exited_output_support;
mod focus_support;
mod hook_identity_support;
mod lifecycle_dispatch_queue;
mod lifecycle_producer_tasks;
mod lifecycle_support;
mod lock_support;
mod mode_tree_support;
mod option_support;
mod overlay_support;
mod pane_output_subscription_rekeys;
mod pane_state_support;
mod pane_stream_support;
mod pane_support;
mod post_commit_sequencer;
mod prompt_support;
mod requester_access;
mod scripting_support;
mod server_access_support;
mod session_lease_support;
mod session_support;
mod shared_handles;
mod shell_processes;
mod shutdown_support;
mod stable_target_identity;
mod theme_support;
mod unix_socket_access_support;
mod web_request_identity;
use shutdown_support::PendingShutdownReason;
pub(crate) use shutdown_support::{DetachedRequestGuard, NormalRequestGuard};
mod sdk_wait_quota;
mod selection_transitions;
mod subscription_support;
mod switch_target_support;
mod target_action_support;
mod target_support;
#[cfg(test)]
#[path = "handler_test_support.rs"]
mod test_support;
mod wait_support;
pub(crate) use wait_support::PreparedSdkWait;
#[cfg(all(any(unix, windows), feature = "web"))]
mod web_support;
#[cfg(not(all(any(unix, windows), feature = "web")))]
mod web_support_disabled;
#[cfg(all(test, any(unix, windows), feature = "web"))]
pub(crate) use web_support::TestWebSessionView;
#[cfg(all(test, any(unix, windows), feature = "web"))]
pub(crate) use web_support::WebSessionPaneView;
#[cfg(all(any(unix, windows), feature = "web"))]
pub(crate) use web_support::{
    UndeliveredWebShareGuard, WebPaneSnapshot, WebPaneStream, WebSessionAttachEvent,
    WebSessionPaneFrame, WebSessionSnapshot, WebSessionStream, WebShareStream,
};
mod window_support;
use crate::pane_state_journal::{PaneStateJournal, PANE_STATE_JOURNAL_CAPACITY};
use crate::pane_terminals::HandlerState;
#[cfg(test)]
use crate::server_access::AccessMode;
use crate::server_access::{current_owner_uid, ServerAccessAdmission, ServerAccessStore};
#[cfg(unix)]
use crate::unix_socket_access::UnixSocketAccessController;
use crate::wait_for::WaitForStore;
#[cfg(all(any(unix, windows), feature = "web"))]
use crate::web::WebShareRegistry;
use attach_support::{ActiveAttachState, ClientFlags};
pub(in crate::handler) use client_environment_support::{
    client_spawn_environment, initial_session_spawn_environment,
};
#[cfg(test)]
pub(in crate::handler) use client_runtime_support::attached_client_name;
pub(crate) use client_runtime_support::with_authenticated_connection_peer;
pub(in crate::handler) use client_runtime_support::{
    attached_client_matches_target, client_environment_snapshot, command_output_from_lines,
    control_client_target_pid, effective_client_terminal_context, format_requester_uid,
    normalize_target_client, parse_client_flags, parse_session_sort_order,
    session_selection_prefers_live_process, sort_list_clients, switch_target_selector_count,
    update_environment_from_client, ListClientSnapshot, SessionSortOrder, LIST_CLIENTS_TEMPLATE,
};
use client_runtime_support::{
    current_process_environment_display_snapshot, current_process_environment_snapshot,
    seed_global_display_environment, seed_global_environment,
};
#[cfg(test)]
pub(in crate::handler) use client_runtime_support::{
    format_attached_client_flags, format_control_client_flags,
};
use clipboard_query_support::ClipboardQueryState;
use control_support::ActiveControlState;
#[cfg(all(test, unix))]
pub(crate) use control_support::ControlRegistrationError;
pub(crate) use control_support::{
    with_control_command_response_sink, with_control_queue_eof_cancellation,
    with_control_queue_identity, ControlClientIdentity, ControlCommandResponseSink,
    ControlQueueDrainLease, ControlQueueEofCancellation, ControlRegistration,
};
use exited_output_support::RetainedExitedPaneOutputs;
pub(in crate::handler) use hook_identity_support::{
    hook_bindings_view, lifecycle_hook_scope_identity, prune_dead_hook_identities,
    resolve_hook_scope_identity, resolve_hook_scope_identity_for_hook,
};
use lifecycle_dispatch_queue::LifecycleDispatchOutbox;
#[cfg(test)]
pub(in crate::handler) use lifecycle_support::after_hook_format_values;
#[cfg(test)]
pub(crate) const TEST_CONTROL_QUEUE_INSERTED_COMMAND_LIMIT: usize =
    scripting_support::CONTROL_QUEUE_INSERTED_COMMAND_LIMIT;
pub(in crate::handler) use lifecycle_support::UnsequencedLifecycleEvent;
pub(in crate::handler) use lifecycle_support::{
    defer_lifecycle_event, prepare_deferred_lifecycle_event, prepare_lifecycle_event,
    prepare_lifecycle_event_if_enabled, prepare_unsequenced_lifecycle_event,
    sequence_prepared_lifecycle_event,
};
pub(crate) use lifecycle_support::{
    DeferredLifecycleEvent, LifecycleDispatchItem, QueuedLifecycleEvent,
    RetainedLifecycleTargetRegistry,
};
use option_support::option_value_u32;
pub(in crate::handler) use pane_output_subscription_rekeys::{
    PaneOutputSubscriptionKeySnapshot, PaneOutputSubscriptionReconciliation,
};
use pane_support::PaneSnapshotRevisionRegistry;
pub(in crate::handler) use requester_access::{
    DetachedRequesterAccess, DetachedRequesterAuthority, DetachedRequesterScope, RequesterOrigin,
};
pub(in crate::handler) use selection_transitions::{
    SelectionTargetTransitionSnapshot, SelectionTransitionSnapshot,
};
use session_lease_support::SessionLeaseStore;
pub(crate) use session_lease_support::{
    with_session_lease_create_addressing, SessionLeaseCreateAddressing,
};
pub(crate) use shared_handles::WeakRequestHandler;
use stable_target_identity::{
    require_expected_stable_pane_identity, require_expected_stable_session_identity,
    require_expected_stable_window_identity, with_expected_stable_target_identities,
    with_expected_synchronized_input_targets, without_expected_stable_target_identities,
    StablePaneOutputIdentity, StableTargetIdentity,
};
use subscription_support::OutputSubscriptionState;
pub(in crate::handler) use switch_target_support::switch_client_target_find_type;
pub(in crate::handler) use target_support::{
    active_session_target, active_window_target, fallback_current_target,
    resolve_existing_session_target, resolve_session_lookup, target_for_request_response,
    target_for_scope_selector, target_to_scope, with_visible_pane_bases, SessionLookup,
};
use wait_support::SdkWaitState;
#[cfg(feature = "web")]
pub(in crate::handler) use web_request_identity::dispatch_with_expected_window_identity;
pub(in crate::handler) use web_request_identity::{
    current_expected_attach_identity, dispatch_with_expected_session_identity,
    dispatch_with_expected_window_occurrence_identity, expected_attach_follows_registration,
    rebase_expected_attach_session_after_switch, require_expected_pane_identity,
    require_expected_session_identity, require_expected_window_identity,
    resolve_expected_window_pane_target, validate_expected_attach_identity,
    with_expected_attach_and_session_identity, with_expected_attach_registration,
    with_expected_session_identity, ExpectedWindowOccurrenceIdentity,
};

/// Default detached session size used when `new-session` omits `-x` and `-y`.
///
/// RMUX currently chooses the conventional 80x24 baseline until client-side
/// terminal discovery is wired in later steps.
pub const DEFAULT_SESSION_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const HOOK_EVENT_BUFFER: usize = 256;
const LIFECYCLE_DISPATCH_BUFFER: usize = 4096;
const PANE_MODE_POST_COMMIT_LIMIT: usize = 16;

pub(in crate::handler) fn current_client_activity_timestamp() -> ClientActivityTimestamp {
    ClientActivityTimestamp::now()
}

#[derive(Debug)]
pub(crate) struct RequestHandler {
    state: Arc<Mutex<HandlerState>>,
    active_attach: Arc<Mutex<ActiveAttachState>>,
    clipboard_queries: Arc<StdMutex<ClipboardQueryState>>,
    active_attach_epoch: Arc<AtomicU64>,
    active_attach_forwarders: Arc<AtomicUsize>,
    client_creation_sequence: Arc<AtomicU64>,
    client_activity_sequence: Arc<AtomicU64>,
    client_size_sequence: Arc<AtomicU64>,
    active_control: Arc<Mutex<ActiveControlState>>,
    silence_timers: Arc<StdMutex<HashMap<WindowTarget, alert_support::SilenceTimerState>>>,
    pane_alert_coalescer: Arc<StdMutex<alert_support::PaneAlertCoalescer>>,
    /// The ordered, bounded sequence of accepted window-rename requests.
    pane_renames: Arc<crate::pane_rename_queue::PaneRenameQueue>,
    pane_alert_dispatch: Arc<Mutex<()>>,
    lifecycle_producers: Arc<lifecycle_producer_tasks::LifecycleProducerRegistry>,
    prompt_history: Arc<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Arc<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
    lifecycle_dispatch: Arc<LifecycleDispatchOutbox<LifecycleDispatchItem>>,
    startup_config_errors: Arc<Mutex<Vec<RmuxError>>>,
    server_socket_path: Arc<StdMutex<PathBuf>>,
    server_access: Arc<StdMutex<ServerAccessStore>>,
    server_access_mutation: Arc<Mutex<()>>,
    #[cfg(unix)]
    unix_socket_access: Arc<StdMutex<Option<UnixSocketAccessController>>>,
    shutdown_requested: Arc<AtomicBool>,
    shutdown_reason: Arc<StdMutex<Option<PendingShutdownReason>>>,
    shutdown_retry_state: Arc<StdMutex<shutdown_support::ShutdownRetryState>>,
    active_detached_connections: Arc<StdMutex<HashSet<u64>>>,
    active_detached_requester_access: Arc<StdMutex<HashMap<u32, DetachedRequesterAccess>>>,
    active_detached_requests: Arc<AtomicUsize>,
    normal_request_admission: Arc<shutdown_support::NormalRequestAdmission>,
    background_tasks: Arc<background_tasks::BackgroundTaskRegistry>,
    shell_processes: Arc<shell_processes::ShellProcessRegistry>,
    shutdown_handle: Arc<StdMutex<Option<ShutdownHandle>>>,
    config_loading_depth: Arc<AtomicUsize>,
    next_connection_id: Arc<AtomicU64>,
    subscriptions: Arc<StdMutex<OutputSubscriptionState>>,
    retained_exited_outputs: Arc<StdMutex<RetainedExitedPaneOutputs>>,
    sdk_waits: Arc<StdMutex<SdkWaitState>>,
    session_leases: Arc<StdMutex<SessionLeaseStore>>,
    session_lease_janitor_started: Arc<AtomicBool>,
    pane_snapshot_coalescers: Arc<StdMutex<PaneSnapshotCoalescerRegistry>>,
    pane_snapshot_revisions: Arc<StdMutex<PaneSnapshotRevisionRegistry>>,
    pane_state_journal: Arc<StdMutex<PaneStateJournal>>,
    pane_state_notify: Arc<Notify>,
    foreground_watch_started: Arc<AtomicBool>,
    foreground_state_cache:
        Arc<StdMutex<HashMap<rmux_core::PaneId, (u64, rmux_proto::ForegroundStateDto)>>>,
    pane_mode_transaction: Arc<Mutex<()>>,
    pane_mode_post_commit: Arc<post_commit_sequencer::PostCommitSequencer>,
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_shares: Arc<WebShareRegistry>,
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_listener_start: Arc<Mutex<()>>,
    task_runtime: Option<tokio::runtime::Handle>,
    #[cfg(test)]
    cleanup_on_drop: bool,
    #[cfg(test)]
    paste_buffer_delete_pause: Arc<StdMutex<Option<Arc<PasteBufferDeletePause>>>>,
    #[cfg(test)]
    window_lifecycle_mutation_pause: Arc<StdMutex<Option<Arc<WindowLifecycleMutationPause>>>>,
    #[cfg(test)]
    window_lifecycle_emit_pause: Arc<StdMutex<Option<Arc<WindowLifecycleEmitPause>>>>,
    #[cfg(test)]
    control_notification_delivery_pause:
        Arc<StdMutex<Option<Arc<ControlNotificationDeliveryPause>>>>,
    #[cfg(test)]
    silence_timer_apply_pause: Arc<StdMutex<Option<Arc<SilenceTimerApplyPause>>>>,
    #[cfg(test)]
    pane_state_lag_rebase_pause: Arc<StdMutex<Option<Arc<PaneStateLagRebasePause>>>>,
    #[cfg(test)]
    pane_option_journal_pause: Arc<StdMutex<Option<Arc<PaneOptionJournalPause>>>>,
    #[cfg(test)]
    pane_exit_commit_pause: Arc<StdMutex<Option<Arc<PaneExitCommitPause>>>>,
    #[cfg(test)]
    initial_pane_tasks: Arc<StdMutex<Vec<tokio::task::JoinHandle<()>>>>,
    #[cfg(test)]
    surface_admission_pause: Arc<StdMutex<Option<Arc<SurfaceAdmissionPause>>>>,
    #[cfg(test)]
    surface_admission_materializations: Arc<AtomicUsize>,
    #[cfg(test)]
    surface_poll_materializations: Arc<AtomicUsize>,
    #[cfg(test)]
    alert_plan_effect_pause: Arc<StdMutex<Option<Arc<AlertPlanEffectPause>>>>,
    #[cfg(test)]
    pane_alert_apply_pause: Arc<StdMutex<Option<Arc<PaneAlertApplyPause>>>>,
    #[cfg(test)]
    pane_exit_rename_pause: Arc<StdMutex<Option<Arc<PaneExitRenamePause>>>>,
    #[cfg(test)]
    attached_size_selection_pause: Arc<StdMutex<Option<Arc<AttachedSizeSelectionPause>>>>,
    #[cfg(test)]
    attached_size_apply_pause: Arc<StdMutex<Option<Arc<AttachedSizeApplyPause>>>>,
    #[cfg(test)]
    attach_registration_activity_pause: Arc<StdMutex<Option<Arc<AttachRegistrationActivityPause>>>>,
    #[cfg(test)]
    attached_mouse_timer_pause: Arc<StdMutex<Option<Arc<pane_support::AttachedMouseTimerPause>>>>,
    #[cfg(test)]
    live_input_work: Arc<pane_support::LiveInputWorkProbe>,
}

pub(crate) struct ConfigLoadingGuard {
    depth: Arc<AtomicUsize>,
}

impl Drop for ConfigLoadingGuard {
    fn drop(&mut self) {
        self.depth.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PasteBufferDeletePause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct WindowLifecycleMutationPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct AttachRegistrationActivityPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct WindowLifecycleEmitPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct ControlNotificationDeliveryPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug)]
struct SilenceTimerApplyPause {
    reached: std::sync::Barrier,
    release: std::sync::Barrier,
}

#[cfg(test)]
impl Default for SilenceTimerApplyPause {
    fn default() -> Self {
        Self {
            reached: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PaneStateLagRebasePause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PaneOptionJournalPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PaneExitCommitPause {
    output_drain_started: tokio::sync::Notify,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct SurfaceAdmissionPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct AlertPlanEffectPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PaneAlertApplyPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

/// Suspends a pane exit between extracting its rename prefix and applying it,
/// so a witness can make an ordinary flush try to take the suffix past it.
#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct PaneExitRenamePause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
impl PaneExitRenamePause {
    pub(in crate::handler) async fn wait_until_reached(&self) {
        self.reached.notified().await;
    }

    pub(in crate::handler) fn release(&self) {
        self.release.notify_one();
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct AttachedSizeSelectionPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct AttachedSizeApplyPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
impl Drop for RequestHandler {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        self.shutdown_background_tasks_for_drop();
        if let Ok(mut state) = self.state.try_lock() {
            state.shutdown_terminals_for_test();
        }
    }
}

impl RequestHandler {
    #[cfg_attr(all(any(unix, windows), feature = "web"), allow(dead_code))]
    pub(crate) fn with_owner_uid_and_subscription_limits(
        owner_uid: u32,
        subscription_limits: SubscriptionLimits,
    ) -> Self {
        Self::with_owner_uid_and_environment_and_display(
            owner_uid,
            Some(current_process_environment_snapshot()),
            Some(current_process_environment_display_snapshot()),
            subscription_limits,
        )
    }

    pub(crate) fn allocate_connection_id(&self) -> u64 {
        self.next_connection_id.fetch_add(1, Ordering::Relaxed)
    }

    /// One creation order for both registries, reserved before a first frame.
    pub(in crate::handler) fn next_client_creation_sequence(&self) -> u64 {
        self.client_creation_sequence
            .fetch_add(1, Ordering::Relaxed)
    }

    pub(in crate::handler) fn next_client_activity_sequence(&self) -> u64 {
        self.client_activity_sequence
            .fetch_add(1, Ordering::Relaxed)
    }

    pub(in crate::handler) fn next_client_size_sequence(&self) -> u64 {
        self.client_size_sequence.fetch_add(1, Ordering::Relaxed)
    }

    pub(in crate::handler) fn current_client_size_sequence(&self) -> u64 {
        self.client_size_sequence.load(Ordering::Relaxed)
    }

    pub(in crate::handler) fn bump_active_attach_epoch(&self) {
        self.active_attach_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn server_task_runtime(&self) -> Option<tokio::runtime::Handle> {
        self.task_runtime.clone()
    }

    pub(crate) fn set_socket_path(&self, socket_path: impl AsRef<Path>) {
        *self
            .server_socket_path
            .lock()
            .expect("server socket path mutex must not be poisoned") =
            socket_path.as_ref().to_path_buf();
    }

    pub(crate) fn socket_path(&self) -> PathBuf {
        self.server_socket_path
            .lock()
            .expect("server socket path mutex must not be poisoned")
            .clone()
    }

    pub(crate) fn start_config_loading(&self) -> ConfigLoadingGuard {
        self.config_loading_depth.fetch_add(1, Ordering::Relaxed);
        ConfigLoadingGuard {
            depth: self.config_loading_depth.clone(),
        }
    }

    pub(crate) fn config_loading_active(&self) -> bool {
        self.config_loading_depth.load(Ordering::Relaxed) != 0
    }

    pub(crate) async fn continue_stopped_panes(&self) {
        #[cfg(unix)]
        {
            self.state.lock().await.continue_stopped_panes();
        }
    }

    pub(crate) fn install_shutdown_handle(&self, shutdown_handle: ShutdownHandle) {
        *self
            .shutdown_handle
            .lock()
            .expect("shutdown handle mutex must not be poisoned") = Some(shutdown_handle);
    }

    pub(crate) async fn shutdown_status_jobs(&self) {
        let status_jobs = self.state.lock().await.status_jobs().clone();
        let _ = tokio::task::spawn_blocking(move || status_jobs.shutdown_and_join()).await;
    }

    #[cfg(test)]
    pub(crate) fn access_mode_for_peer(&self, peer: &PeerIdentity) -> Option<AccessMode> {
        self.server_access
            .lock()
            .ok()
            .and_then(|server_access| server_access.mode_for_identity(&peer.user))
    }

    pub(crate) fn server_access_admission_for_peer(
        &self,
        peer: &PeerIdentity,
    ) -> Option<ServerAccessAdmission> {
        self.server_access
            .lock()
            .ok()
            .and_then(|server_access| server_access.admission_for_identity(&peer.user))
    }

    pub(crate) fn server_access_admission_is_current(
        &self,
        peer: &PeerIdentity,
        admission: &ServerAccessAdmission,
    ) -> bool {
        self.server_access
            .lock()
            .ok()
            .and_then(|server_access| server_access.revalidate_admission(admission, &peer.user))
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn set_test_access_mode_for_uid(
        &self,
        uid: u32,
        mode: AccessMode,
    ) -> Result<(), RmuxError> {
        self.server_access
            .lock()
            .expect("server access mutex must not be poisoned")
            .set_mode(uid, mode)
    }

    #[cfg(test)]
    pub(crate) fn remove_test_access_for_uid(&self, uid: u32) -> Result<(), RmuxError> {
        self.server_access
            .lock()
            .expect("server access mutex must not be poisoned")
            .remove_uid(uid)
    }

    #[cfg(test)]
    fn install_paste_buffer_delete_pause(&self) -> Arc<PasteBufferDeletePause> {
        let pause = Arc::new(PasteBufferDeletePause::default());
        *self
            .paste_buffer_delete_pause
            .lock()
            .expect("paste-buffer delete pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_paste_buffer_delete(&self) {
        let pause = self
            .paste_buffer_delete_pause
            .lock()
            .expect("paste-buffer delete pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_paste_buffer_delete(&self) {}

    #[cfg(test)]
    fn install_window_lifecycle_mutation_pause(&self) -> Arc<WindowLifecycleMutationPause> {
        let pause = Arc::new(WindowLifecycleMutationPause::default());
        *self
            .window_lifecycle_mutation_pause
            .lock()
            .expect("window lifecycle mutation pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_window_lifecycle_mutation(&self) {
        let pause = self
            .window_lifecycle_mutation_pause
            .lock()
            .expect("window lifecycle mutation pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_window_lifecycle_mutation(&self) {}

    #[cfg(test)]
    fn install_window_lifecycle_emit_pause(&self) -> Arc<WindowLifecycleEmitPause> {
        let pause = Arc::new(WindowLifecycleEmitPause::default());
        *self
            .window_lifecycle_emit_pause
            .lock()
            .expect("window lifecycle emit pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_window_lifecycle_emit(&self) {
        let pause = self
            .window_lifecycle_emit_pause
            .lock()
            .expect("window lifecycle emit pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_window_lifecycle_emit(&self) {}

    #[cfg(test)]
    fn install_alert_plan_effect_pause(&self) -> Arc<AlertPlanEffectPause> {
        let pause = Arc::new(AlertPlanEffectPause::default());
        *self
            .alert_plan_effect_pause
            .lock()
            .expect("alert plan effect pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_after_alert_plan_hook_enqueue(&self) {
        let pause = self
            .alert_plan_effect_pause
            .lock()
            .expect("alert plan effect pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_after_alert_plan_hook_enqueue(&self) {}

    #[cfg(test)]
    pub(in crate::handler) fn install_pane_exit_rename_pause(&self) -> Arc<PaneExitRenamePause> {
        let pause = Arc::new(PaneExitRenamePause::default());
        *self
            .pane_exit_rename_pause
            .lock()
            .expect("pane exit rename pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(in crate::handler) async fn pause_at_pane_exit_rename_boundary(&self) {
        let pause = self
            .pane_exit_rename_pause
            .lock()
            .expect("pane exit rename pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_at_pane_exit_rename_boundary(&self) {}

    #[cfg(test)]
    fn install_pane_alert_apply_pause(&self) -> Arc<PaneAlertApplyPause> {
        let pause = Arc::new(PaneAlertApplyPause::default());
        *self
            .pane_alert_apply_pause
            .lock()
            .expect("pane alert apply pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_pane_alert_final_apply(&self) {
        let pause = self
            .pane_alert_apply_pause
            .lock()
            .expect("pane alert apply pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_pane_alert_final_apply(&self) {}

    #[cfg(test)]
    fn install_attached_size_selection_pause(&self) -> Arc<AttachedSizeSelectionPause> {
        let pause = Arc::new(AttachedSizeSelectionPause::default());
        *self
            .attached_size_selection_pause
            .lock()
            .expect("attached size selection pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_after_attached_size_selection(&self) {
        let pause = self
            .attached_size_selection_pause
            .lock()
            .expect("attached size selection pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_after_attached_size_selection(&self) {}

    #[cfg(test)]
    fn install_attached_size_apply_pause(&self) -> Arc<AttachedSizeApplyPause> {
        let pause = Arc::new(AttachedSizeApplyPause::default());
        *self
            .attached_size_apply_pause
            .lock()
            .expect("attached size apply pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_attached_size_apply(&self) {
        let pause = self
            .attached_size_apply_pause
            .lock()
            .expect("attached size apply pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_attached_size_apply(&self) {}

    #[cfg(test)]
    fn install_attach_registration_activity_pause(&self) -> Arc<AttachRegistrationActivityPause> {
        let pause = Arc::new(AttachRegistrationActivityPause::default());
        *self
            .attach_registration_activity_pause
            .lock()
            .expect("attach registration activity pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_attach_registration_activity(&self) {
        let pause = self
            .attach_registration_activity_pause
            .lock()
            .expect("attach registration activity pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_attach_registration_activity(&self) {}

    #[cfg(test)]
    fn install_silence_timer_apply_pause(&self) -> Arc<SilenceTimerApplyPause> {
        let pause = Arc::new(SilenceTimerApplyPause::default());
        *self
            .silence_timer_apply_pause
            .lock()
            .expect("silence timer apply pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    fn pause_before_silence_timer_apply(&self) {
        let pause = self
            .silence_timer_apply_pause
            .lock()
            .expect("silence timer apply pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.wait();
            pause.release.wait();
        }
    }

    #[cfg(test)]
    fn install_pane_state_lag_rebase_pause(&self) -> Arc<PaneStateLagRebasePause> {
        let pause = Arc::new(PaneStateLagRebasePause::default());
        *self
            .pane_state_lag_rebase_pause
            .lock()
            .expect("pane state lag rebase pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_pane_state_lag_snapshot(&self) {
        let pause = self
            .pane_state_lag_rebase_pause
            .lock()
            .expect("pane state lag rebase pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_pane_state_lag_snapshot(&self) {}

    #[cfg(test)]
    fn install_pane_option_journal_pause(&self) -> Arc<PaneOptionJournalPause> {
        let pause = Arc::new(PaneOptionJournalPause::default());
        *self
            .pane_option_journal_pause
            .lock()
            .expect("pane option journal pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_pane_option_journal(&self) {
        let pause = self
            .pane_option_journal_pause
            .lock()
            .expect("pane option journal pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_before_pane_option_journal(&self) {}

    #[cfg(test)]
    fn install_pane_exit_commit_pause(&self) -> Arc<PaneExitCommitPause> {
        let pause = Arc::new(PaneExitCommitPause::default());
        *self
            .pane_exit_commit_pause
            .lock()
            .expect("pane exit commit pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    fn notify_pane_exit_output_drain_started(&self) {
        let pause = self
            .pane_exit_commit_pause
            .lock()
            .expect("pane exit commit pause")
            .clone();
        if let Some(pause) = pause {
            pause.output_drain_started.notify_one();
        }
    }

    #[cfg(not(test))]
    fn notify_pane_exit_output_drain_started(&self) {}

    #[cfg(test)]
    async fn pause_after_pane_exit_commit(&self) {
        let pause = self
            .pane_exit_commit_pause
            .lock()
            .expect("pane exit commit pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_after_pane_exit_commit(&self) {}

    // Fixture setup must join the startup refresh too: leaving Starting does
    // not mean the deferred task has finished publishing attach controls.
    #[cfg(test)]
    async fn wait_for_initial_pane_tasks_for_test(&self) {
        let tasks =
            std::mem::take(&mut *self.initial_pane_tasks.lock().expect("initial pane tasks"));
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            for task in tasks {
                task.await.expect("initial pane task completes");
            }
        })
        .await
        .expect("initial pane tasks finish before fixture observation");
    }

    #[cfg(test)]
    async fn wait_for_initial_panes_for_test(&self) {
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_panes_ready().await;
    }
}

#[cfg(test)]
#[path = "handler_send_keys_tests/input_capture.rs"]
mod input_capture;

// The G4 observations of the visible surface's coordinates live with the mouse
// modules they describe and are declared here, in the module that owns the
// attached-input helpers they read. This declaration is this lane's own and is
// separate from any other test module of this file.
#[cfg(all(test, unix))]
#[path = "mouse/visible_surface_coordinates_g4_tests.rs"]
mod visible_surface_coordinates_g4_tests;

// The remaining visible-surface observations of this lane: the right
// scrollbar and its track, a deeper vertical viewport, and floats placed at a
// signed logical origin. Declared separately from the ones above.
#[cfg(all(test, unix))]
#[path = "mouse/visible_surface_remaining_g4_tests.rs"]
mod visible_surface_remaining_g4_tests;

// The G3 application-mouse observations live with the mouse modules they
// describe and are declared here, in the module that owns the attached-input
// helpers they read.
#[cfg(all(test, unix))]
#[path = "mouse/application_mouse_g3_tests.rs"]
mod application_mouse_g3_tests;

// The G6 multiple-links observation: the real ordinary link through the complete handler, then what
// each winlink resolves to. Declared here because RequestHandler's state is private to this module.
#[cfg(test)]
#[path = "handler_g6_multiple_links_tests.rs"]
mod g6_multiple_links_tests;

// The G6 scene-revision observation: mouse delivery before, during and after an ordinary Status
// change. Declared here for the same reason as the module above - RequestHandler's state is private
// to this module.
#[cfg(test)]
#[path = "handler_g6_scene_revision_tests.rs"]
mod g6_scene_revision_tests;

#[cfg(test)]
mod frozen_options_tests;

#[cfg(test)]
#[path = "handler_tcol_tests.rs"]
mod tcol_tests;

#[cfg(test)]
#[path = "handler_mp1_tests.rs"]
mod mp1_tests;

#[cfg(test)]
#[path = "handler_op1_tests.rs"]
mod op1_tests;

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "handler_server_access_transport_tests.rs"]
mod server_access_transport_tests;

#[cfg(test)]
#[path = "handler_attach_tests.rs"]
mod attach_tests;

#[cfg(test)]
#[path = "handler_window_tests.rs"]
mod window_tests;

#[cfg(test)]
#[path = "handler_wgi1_tests.rs"]
mod wgi1_tests;

#[cfg(test)]
#[path = "handler_set_mutation_tests.rs"]
mod set_mutation_tests;

#[cfg(test)]
#[path = "handler_environment_hook_tests.rs"]
mod environment_hook_tests;

#[cfg(test)]
#[path = "handler_hook_dispatch_tests.rs"]
mod hook_dispatch_tests;

#[cfg(test)]
#[path = "handler_hook_identity_tests.rs"]
mod hook_identity_tests;

#[cfg(test)]
#[path = "handler_lifecycle_target_tests.rs"]
mod lifecycle_target_tests;

#[cfg(test)]
#[path = "handler_zoom_tests.rs"]
mod zoom_tests;

#[cfg(test)]
#[path = "handler_layout_tests.rs"]
mod layout_tests;

#[cfg(test)]
#[path = "handler_layout_notification_tests.rs"]
mod layout_notification_tests;

#[cfg(test)]
#[path = "handler_show_tests.rs"]
mod show_tests;

#[cfg(test)]
#[path = "handler_buffer_tests.rs"]
mod buffer_tests;

#[cfg(test)]
#[path = "handler_clipboard_query_tests.rs"]
mod clipboard_query_tests;

#[cfg(test)]
#[path = "handler_capture_tests.rs"]
mod capture_tests;

#[cfg(test)]
#[path = "handler_display_message_tests.rs"]
mod display_message_tests;

#[cfg(test)]
#[path = "handler_alert_tests.rs"]
mod alert_tests;

#[cfg(test)]
#[path = "handler_auto_rename_notification_tests.rs"]
mod auto_rename_notification_tests;

#[cfg(test)]
#[path = "handler_arn1_tests.rs"]
mod arn1_tests;

#[cfg(test)]
#[path = "handler_winlink_insertion_tests.rs"]
mod winlink_insertion_tests;

#[cfg(test)]
#[path = "handler_pane_alert_race_tests.rs"]
mod pane_alert_race_tests;

#[cfg(test)]
#[path = "handler_clock_mode_identity_tests.rs"]
mod clock_mode_identity_tests;
#[cfg(test)]
#[path = "handler_clock_mode_tests.rs"]
mod clock_mode_tests;

#[cfg(test)]
#[path = "handler_control_notification_tests.rs"]
mod control_notification_tests;

#[cfg(test)]
#[path = "handler_selection_control_notification_tests.rs"]
mod selection_control_notification_tests;

#[cfg(test)]
#[path = "handler_explicit_kill_selection_tests.rs"]
mod explicit_kill_selection_tests;

#[cfg(test)]
#[path = "handler_switch_selection_notification_tests.rs"]
mod switch_selection_notification_tests;

#[cfg(test)]
#[path = "handler_natural_exit_selection_tests.rs"]
mod natural_exit_selection_tests;

#[cfg(test)]
#[path = "handler_control_lifecycle_tests.rs"]
mod control_lifecycle_tests;

#[cfg(test)]
#[path = "handler_control_delivery_failure_tests.rs"]
mod control_delivery_failure_tests;

#[cfg(test)]
#[path = "handler_scripting_tests.rs"]
mod scripting_tests;

#[cfg(test)]
#[path = "handler_prompt_tests.rs"]
mod prompt_tests;

#[cfg(test)]
#[path = "handler_pane_command_tests.rs"]
mod pane_command_tests;

#[cfg(test)]
#[path = "handler_default_command_tests.rs"]
mod default_command_tests;

#[cfg(test)]
#[path = "handler_pane_family_lifecycle_tests.rs"]
mod pane_family_lifecycle_tests;

#[cfg(test)]
#[path = "handler_pane_float_transfer_tests.rs"]
mod pane_float_transfer_tests;
#[cfg(test)]
#[path = "handler_pane_group_linked_transfer_tests.rs"]
mod pane_group_linked_transfer_tests;
#[cfg(test)]
#[path = "handler_pane_group_refresh_tests.rs"]
mod pane_group_refresh_tests;
#[cfg(test)]
#[path = "handler_pane_group_transfer_tests.rs"]
mod pane_group_transfer_tests;
#[cfg(test)]
#[path = "handler_pane_transfer_hook_tests.rs"]
mod pane_transfer_hook_tests;
#[cfg(test)]
#[path = "handler_pane_transfer_renumber_tests.rs"]
mod pane_transfer_renumber_tests;
#[cfg(test)]
#[path = "handler_pane_transfer_scroll_on_clear_tests.rs"]
mod pane_transfer_scroll_on_clear_tests;
#[cfg(test)]
#[path = "handler_pane_window_metadata_tests.rs"]
mod pane_window_metadata_tests;

#[cfg(test)]
#[path = "handler_pane_pipe_tests.rs"]
mod pane_pipe_tests;

#[cfg(test)]
#[path = "handler_pane_exit_format_tests.rs"]
mod pane_exit_format_tests;

#[cfg(test)]
#[path = "handler_pane_unseen_changes_tests.rs"]
mod pane_unseen_changes_tests;

#[cfg(test)]
#[path = "handler_cquit_tests.rs"]
mod cquit_tests;

#[cfg(test)]
#[path = "handler_pane_silence_timer_tests.rs"]
mod pane_silence_timer_tests;

#[cfg(test)]
#[path = "handler_pane_state_tests.rs"]
mod pane_state_tests;

#[cfg(all(test, unix))]
#[path = "handler_pane_shell_tests.rs"]
mod pane_shell_tests;

#[cfg(test)]
#[path = "handler_pane_state_race_tests.rs"]
mod pane_state_race_tests;

#[cfg(test)]
#[path = "handler_request_identity_tests.rs"]
mod request_identity_tests;

#[cfg(test)]
#[path = "handler_rotate_pane_options_tests.rs"]
mod rotate_pane_options_tests;

#[cfg(test)]
#[path = "handler_pane_alias_lifecycle_tests.rs"]
mod pane_alias_lifecycle_tests;

#[cfg(test)]
#[path = "handler_linked_pane_kill_tests.rs"]
mod linked_pane_kill_tests;

#[cfg(test)]
#[path = "handler_floating_surface_tests.rs"]
mod floating_surface_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "handler_native_resize_outcome_tests.rs"]
mod native_resize_outcome_tests;
