use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, Weak};

use rmux_core::events::{PaneSnapshotCoalescerRegistry, SubscriptionLimits};
use rmux_ipc::PeerIdentity;
use rmux_proto::{RmuxError, TerminalSize, WindowTarget};
use tokio::sync::{broadcast, Mutex, Notify};

use crate::daemon::ShutdownHandle;
#[path = "handler_alerts.rs"]
mod alert_support;
#[path = "handler_attach.rs"]
pub(crate) mod attach_support;
#[path = "handler_buffer.rs"]
mod buffer_support;
#[path = "handler_client_environment.rs"]
mod client_environment_support;
#[path = "handler_client_runtime.rs"]
mod client_runtime_support;
#[path = "handler_client.rs"]
mod client_support;
#[path = "handler_clock_mode.rs"]
mod clock_mode_support;
#[path = "handler_config.rs"]
mod config_support;
#[path = "handler_control.rs"]
mod control_support;
#[path = "handler_copy_mode.rs"]
mod copy_mode_support;
#[path = "handler_daemon.rs"]
mod daemon_support;
#[path = "handler_dispatch.rs"]
mod dispatch_support;
#[path = "handler_exited_outputs.rs"]
mod exited_output_support;
#[path = "handler_hook_identity.rs"]
mod hook_identity_support;
#[path = "handler/lifecycle_dispatch_queue.rs"]
mod lifecycle_dispatch_queue;
#[path = "handler_lifecycle.rs"]
mod lifecycle_support;
#[path = "handler_lock.rs"]
mod lock_support;
#[path = "handler_mode_tree.rs"]
mod mode_tree_support;
#[path = "handler_options.rs"]
mod option_support;
#[path = "handler_overlay.rs"]
mod overlay_support;
#[path = "handler/pane_output_subscription_rekeys.rs"]
mod pane_output_subscription_rekeys;
#[path = "handler_pane_state.rs"]
mod pane_state_support;
#[path = "handler_pane.rs"]
mod pane_support;
#[path = "handler_prompt.rs"]
mod prompt_support;
#[path = "handler_scripting.rs"]
mod scripting_support;
#[path = "handler_server_access.rs"]
mod server_access_support;
#[path = "handler_session/leases.rs"]
mod session_lease_support;
#[path = "handler_session.rs"]
mod session_support;
#[path = "handler_shutdown.rs"]
mod shutdown_support;
#[path = "handler/web_request_identity.rs"]
mod web_request_identity;
pub(crate) use shutdown_support::DetachedRequestGuard;
#[path = "handler_subscriptions.rs"]
mod subscription_support;
#[path = "handler_switch_target.rs"]
mod switch_target_support;
#[path = "handler_target_actions.rs"]
mod target_action_support;
#[path = "handler_targets.rs"]
mod target_support;
#[cfg(test)]
#[path = "handler_test_support.rs"]
mod test_support;
#[path = "handler_waits.rs"]
mod wait_support;
pub(crate) use wait_support::PreparedSdkWait;
#[cfg(all(any(unix, windows), feature = "web"))]
#[path = "handler_web.rs"]
mod web_support;
#[cfg(not(all(any(unix, windows), feature = "web")))]
#[path = "handler_web_disabled.rs"]
mod web_support;
#[cfg(all(test, any(unix, windows), feature = "web"))]
pub(crate) use web_support::TestWebSessionView;
#[cfg(all(test, any(unix, windows), feature = "web"))]
pub(crate) use web_support::WebSessionPaneView;
#[cfg(all(any(unix, windows), feature = "web"))]
pub(crate) use web_support::{
    WebPaneSnapshot, WebPaneStream, WebSessionAttachEvent, WebSessionPaneFrame, WebSessionSnapshot,
    WebSessionStream, WebShareStream,
};
#[path = "handler_window.rs"]
mod window_support;
use crate::pane_state_journal::{PaneStateJournal, PANE_STATE_JOURNAL_CAPACITY};
use crate::pane_terminals::HandlerState;
use crate::server_access::{current_owner_uid, AccessMode, ServerAccessStore};
use crate::wait_for::WaitForStore;
#[cfg(all(any(unix, windows), feature = "web"))]
use crate::web::WebShareRegistry;
use attach_support::{ActiveAttachState, ClientFlags};
pub(in crate::handler) use client_environment_support::{
    client_spawn_environment, initial_session_spawn_environment,
};
pub(in crate::handler) use client_runtime_support::{
    attached_client_matches_target, attached_client_name, client_environment_snapshot,
    command_output_from_lines, effective_client_terminal_context, format_client_uid,
    format_client_user, format_requester_uid, normalize_target_client, parse_client_flags,
    parse_session_sort_order, session_selection_prefers_live_process, sort_list_clients,
    switch_target_selector_count, update_environment_from_client, ListClientSnapshot,
    SessionSortOrder, LIST_CLIENTS_TEMPLATE,
};
use client_runtime_support::{
    current_process_environment_display_snapshot, current_process_environment_snapshot,
    seed_global_display_environment, seed_global_environment,
};
#[cfg(test)]
pub(in crate::handler) use client_runtime_support::{
    format_attached_client_flags, format_control_client_flags,
};
use control_support::ActiveControlState;
#[cfg(all(test, unix))]
pub(crate) use control_support::ControlRegistrationError;
pub(crate) use control_support::{
    with_control_queue_eof_cancellation, with_control_queue_identity, ControlClientIdentity,
    ControlQueueDrainLease, ControlQueueEofCancellation, ControlRegistration,
};
use exited_output_support::RetainedExitedPaneOutputs;
pub(in crate::handler) use hook_identity_support::{
    hook_bindings_view, lifecycle_hook_scope_identity, prune_dead_hook_identities,
    resolve_hook_scope_identity, resolve_hook_scope_identity_for_hook,
};
use lifecycle_dispatch_queue::BoundedDispatchQueue;
#[cfg(test)]
pub(in crate::handler) use lifecycle_support::after_hook_format_values;
pub(in crate::handler) use lifecycle_support::{
    defer_lifecycle_event, prepare_deferred_lifecycle_event, prepare_lifecycle_event,
    prepare_lifecycle_event_if_enabled,
};
pub(crate) use lifecycle_support::{
    DeferredLifecycleEvent, LifecycleDispatchItem, QueuedLifecycleEvent,
};
use option_support::option_value_u32;
pub(in crate::handler) use pane_output_subscription_rekeys::{
    PaneOutputSubscriptionKeySnapshot, PaneOutputSubscriptionReconciliation,
};
use pane_support::PaneSnapshotRevisionRegistry;
use session_lease_support::SessionLeaseStore;
pub(crate) use session_lease_support::{
    with_session_lease_create_addressing, SessionLeaseCreateAddressing,
};
use subscription_support::OutputSubscriptionState;
pub(in crate::handler) use switch_target_support::switch_client_target_find_type;
pub(in crate::handler) use target_support::{
    active_session_target, active_window_target, fallback_current_target,
    resolve_existing_session_target, resolve_session_lookup, target_for_request_response,
    target_for_scope_selector, target_to_scope, with_visible_pane_bases, SessionLookup,
};
use wait_support::SdkWaitState;
pub(in crate::handler) use web_request_identity::{
    current_expected_attach_identity, dispatch_with_expected_session_identity,
    dispatch_with_expected_window_identity, dispatch_with_expected_window_occurrence_identity,
    expected_attach_follows_registration, rebase_expected_attach_session_after_switch,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum PendingShutdownReason {
    ExitEmpty,
    KillServer,
    SeamlessUpgradeIdle,
}

#[derive(Debug, Default)]
pub(in crate::handler) struct DetachedRequesterAccess {
    write_scopes: usize,
    read_only_scopes: usize,
}

impl DetachedRequesterAccess {
    pub(in crate::handler) fn can_write(&self) -> bool {
        self.write_scopes > 0
    }

    pub(in crate::handler) fn is_empty(&self) -> bool {
        self.write_scopes == 0 && self.read_only_scopes == 0
    }
}

#[derive(Debug)]
pub(crate) struct RequestHandler {
    state: Arc<Mutex<HandlerState>>,
    active_attach: Arc<Mutex<ActiveAttachState>>,
    active_attach_epoch: Arc<AtomicU64>,
    active_attach_forwarders: Arc<AtomicUsize>,
    active_control: Arc<Mutex<ActiveControlState>>,
    silence_timers: Arc<StdMutex<HashMap<WindowTarget, alert_support::SilenceTimerState>>>,
    pane_alert_coalescer: Arc<StdMutex<alert_support::PaneAlertCoalescer>>,
    pane_alert_dispatch: Arc<Mutex<()>>,
    prompt_history: Arc<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Arc<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
    lifecycle_dispatch: Arc<BoundedDispatchQueue<LifecycleDispatchItem>>,
    startup_config_errors: Arc<Mutex<Vec<RmuxError>>>,
    server_socket_path: Arc<StdMutex<PathBuf>>,
    server_access: Arc<StdMutex<ServerAccessStore>>,
    shutdown_requested: Arc<AtomicBool>,
    shutdown_reason: Arc<StdMutex<Option<PendingShutdownReason>>>,
    shutdown_retry_scheduled: Arc<AtomicBool>,
    active_detached_connections: Arc<StdMutex<HashSet<u64>>>,
    active_detached_requester_access: Arc<StdMutex<HashMap<u32, DetachedRequesterAccess>>>,
    active_detached_requests: Arc<AtomicUsize>,
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
    alert_plan_effect_pause: Arc<StdMutex<Option<Arc<AlertPlanEffectPause>>>>,
    #[cfg(test)]
    pane_alert_apply_pause: Arc<StdMutex<Option<Arc<PaneAlertApplyPause>>>>,
    #[cfg(test)]
    attached_size_selection_pause: Arc<StdMutex<Option<Arc<AttachedSizeSelectionPause>>>>,
    #[cfg(test)]
    attached_size_apply_pause: Arc<StdMutex<Option<Arc<AttachedSizeApplyPause>>>>,
}

pub(crate) struct ConfigLoadingGuard {
    depth: Arc<AtomicUsize>,
}

impl Drop for ConfigLoadingGuard {
    fn drop(&mut self) {
        self.depth.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Clone for RequestHandler {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            active_attach: self.active_attach.clone(),
            active_attach_epoch: self.active_attach_epoch.clone(),
            active_attach_forwarders: self.active_attach_forwarders.clone(),
            active_control: self.active_control.clone(),
            silence_timers: self.silence_timers.clone(),
            pane_alert_coalescer: self.pane_alert_coalescer.clone(),
            pane_alert_dispatch: self.pane_alert_dispatch.clone(),
            prompt_history: self.prompt_history.clone(),
            wait_for: self.wait_for.clone(),
            hook_events: self.hook_events.clone(),
            lifecycle_dispatch: self.lifecycle_dispatch.clone(),
            startup_config_errors: self.startup_config_errors.clone(),
            server_socket_path: self.server_socket_path.clone(),
            server_access: self.server_access.clone(),
            shutdown_requested: self.shutdown_requested.clone(),
            shutdown_reason: self.shutdown_reason.clone(),
            shutdown_retry_scheduled: self.shutdown_retry_scheduled.clone(),
            active_detached_connections: self.active_detached_connections.clone(),
            active_detached_requester_access: self.active_detached_requester_access.clone(),
            active_detached_requests: self.active_detached_requests.clone(),
            shutdown_handle: self.shutdown_handle.clone(),
            config_loading_depth: self.config_loading_depth.clone(),
            next_connection_id: self.next_connection_id.clone(),
            subscriptions: self.subscriptions.clone(),
            retained_exited_outputs: self.retained_exited_outputs.clone(),
            sdk_waits: self.sdk_waits.clone(),
            session_leases: self.session_leases.clone(),
            session_lease_janitor_started: self.session_lease_janitor_started.clone(),
            pane_snapshot_coalescers: self.pane_snapshot_coalescers.clone(),
            pane_snapshot_revisions: self.pane_snapshot_revisions.clone(),
            pane_state_journal: self.pane_state_journal.clone(),
            pane_state_notify: self.pane_state_notify.clone(),
            foreground_watch_started: self.foreground_watch_started.clone(),
            foreground_state_cache: self.foreground_state_cache.clone(),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: self.web_shares.clone(),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: self.web_listener_start.clone(),
            task_runtime: self.task_runtime.clone(),
            #[cfg(test)]
            cleanup_on_drop: false,
            #[cfg(test)]
            paste_buffer_delete_pause: self.paste_buffer_delete_pause.clone(),
            #[cfg(test)]
            window_lifecycle_mutation_pause: self.window_lifecycle_mutation_pause.clone(),
            #[cfg(test)]
            window_lifecycle_emit_pause: self.window_lifecycle_emit_pause.clone(),
            #[cfg(test)]
            control_notification_delivery_pause: self.control_notification_delivery_pause.clone(),
            #[cfg(test)]
            silence_timer_apply_pause: self.silence_timer_apply_pause.clone(),
            #[cfg(test)]
            pane_state_lag_rebase_pause: self.pane_state_lag_rebase_pause.clone(),
            #[cfg(test)]
            pane_option_journal_pause: self.pane_option_journal_pause.clone(),
            #[cfg(test)]
            pane_exit_commit_pause: self.pane_exit_commit_pause.clone(),
            #[cfg(test)]
            alert_plan_effect_pause: self.alert_plan_effect_pause.clone(),
            #[cfg(test)]
            pane_alert_apply_pause: self.pane_alert_apply_pause.clone(),
            #[cfg(test)]
            attached_size_selection_pause: self.attached_size_selection_pause.clone(),
            #[cfg(test)]
            attached_size_apply_pause: self.attached_size_apply_pause.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct WeakRequestHandler {
    state: Weak<Mutex<HandlerState>>,
    active_attach: Weak<Mutex<ActiveAttachState>>,
    active_attach_epoch: Weak<AtomicU64>,
    active_attach_forwarders: Weak<AtomicUsize>,
    active_control: Weak<Mutex<ActiveControlState>>,
    silence_timers: Weak<StdMutex<HashMap<WindowTarget, alert_support::SilenceTimerState>>>,
    pane_alert_coalescer: Weak<StdMutex<alert_support::PaneAlertCoalescer>>,
    pane_alert_dispatch: Weak<Mutex<()>>,
    prompt_history: Weak<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Weak<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
    lifecycle_dispatch: Weak<BoundedDispatchQueue<LifecycleDispatchItem>>,
    startup_config_errors: Weak<Mutex<Vec<RmuxError>>>,
    server_socket_path: Weak<StdMutex<PathBuf>>,
    server_access: Weak<StdMutex<ServerAccessStore>>,
    shutdown_requested: Weak<AtomicBool>,
    shutdown_reason: Weak<StdMutex<Option<PendingShutdownReason>>>,
    shutdown_retry_scheduled: Weak<AtomicBool>,
    active_detached_connections: Weak<StdMutex<HashSet<u64>>>,
    active_detached_requester_access: Weak<StdMutex<HashMap<u32, DetachedRequesterAccess>>>,
    active_detached_requests: Weak<AtomicUsize>,
    shutdown_handle: Weak<StdMutex<Option<ShutdownHandle>>>,
    config_loading_depth: Weak<AtomicUsize>,
    next_connection_id: Weak<AtomicU64>,
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    retained_exited_outputs: Weak<StdMutex<RetainedExitedPaneOutputs>>,
    sdk_waits: Weak<StdMutex<SdkWaitState>>,
    session_leases: Weak<StdMutex<SessionLeaseStore>>,
    session_lease_janitor_started: Weak<AtomicBool>,
    pane_snapshot_coalescers: Weak<StdMutex<PaneSnapshotCoalescerRegistry>>,
    pane_snapshot_revisions: Weak<StdMutex<PaneSnapshotRevisionRegistry>>,
    pane_state_journal: Weak<StdMutex<PaneStateJournal>>,
    pane_state_notify: Weak<Notify>,
    foreground_watch_started: Weak<AtomicBool>,
    foreground_state_cache:
        Weak<StdMutex<HashMap<rmux_core::PaneId, (u64, rmux_proto::ForegroundStateDto)>>>,
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_shares: Weak<WebShareRegistry>,
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_listener_start: Weak<Mutex<()>>,
    task_runtime: Option<tokio::runtime::Handle>,
    #[cfg(test)]
    paste_buffer_delete_pause: Weak<StdMutex<Option<Arc<PasteBufferDeletePause>>>>,
}

impl WeakRequestHandler {
    pub(crate) fn upgrade(&self) -> Option<RequestHandler> {
        Some(RequestHandler {
            state: self.state.upgrade()?,
            active_attach: self.active_attach.upgrade()?,
            active_attach_epoch: self.active_attach_epoch.upgrade()?,
            active_attach_forwarders: self.active_attach_forwarders.upgrade()?,
            active_control: self.active_control.upgrade()?,
            silence_timers: self.silence_timers.upgrade()?,
            pane_alert_coalescer: self.pane_alert_coalescer.upgrade()?,
            pane_alert_dispatch: self.pane_alert_dispatch.upgrade()?,
            prompt_history: self.prompt_history.upgrade()?,
            wait_for: self.wait_for.upgrade()?,
            hook_events: self.hook_events.clone(),
            lifecycle_dispatch: self.lifecycle_dispatch.upgrade()?,
            startup_config_errors: self.startup_config_errors.upgrade()?,
            server_socket_path: self.server_socket_path.upgrade()?,
            server_access: self.server_access.upgrade()?,
            shutdown_requested: self.shutdown_requested.upgrade()?,
            shutdown_reason: self.shutdown_reason.upgrade()?,
            shutdown_retry_scheduled: self.shutdown_retry_scheduled.upgrade()?,
            active_detached_connections: self.active_detached_connections.upgrade()?,
            active_detached_requester_access: self.active_detached_requester_access.upgrade()?,
            active_detached_requests: self.active_detached_requests.upgrade()?,
            shutdown_handle: self.shutdown_handle.upgrade()?,
            config_loading_depth: self.config_loading_depth.upgrade()?,
            next_connection_id: self.next_connection_id.upgrade()?,
            subscriptions: self.subscriptions.upgrade()?,
            retained_exited_outputs: self.retained_exited_outputs.upgrade()?,
            sdk_waits: self.sdk_waits.upgrade()?,
            session_leases: self.session_leases.upgrade()?,
            session_lease_janitor_started: self.session_lease_janitor_started.upgrade()?,
            pane_snapshot_coalescers: self.pane_snapshot_coalescers.upgrade()?,
            pane_snapshot_revisions: self.pane_snapshot_revisions.upgrade()?,
            pane_state_journal: self.pane_state_journal.upgrade()?,
            pane_state_notify: self.pane_state_notify.upgrade()?,
            foreground_watch_started: self.foreground_watch_started.upgrade()?,
            foreground_state_cache: self.foreground_state_cache.upgrade()?,
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: self.web_shares.upgrade()?,
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: self.web_listener_start.upgrade()?,
            task_runtime: self.task_runtime.clone(),
            #[cfg(test)]
            cleanup_on_drop: false,
            #[cfg(test)]
            paste_buffer_delete_pause: self.paste_buffer_delete_pause.upgrade()?,
            #[cfg(test)]
            window_lifecycle_mutation_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            window_lifecycle_emit_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            control_notification_delivery_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            silence_timer_apply_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_state_lag_rebase_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_option_journal_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_exit_commit_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            alert_plan_effect_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_alert_apply_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attached_size_selection_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attached_size_apply_pause: Arc::new(StdMutex::new(None)),
        })
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

impl Default for RequestHandler {
    fn default() -> Self {
        Self::with_owner_uid(current_owner_uid())
    }
}

#[cfg(test)]
impl Drop for RequestHandler {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        if let Ok(mut state) = self.state.try_lock() {
            state.shutdown_terminals_for_test();
        }
    }
}

impl RequestHandler {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_owner_uid_and_environment(
            current_owner_uid(),
            None,
            SubscriptionLimits::default(),
        )
    }

    pub(crate) fn with_owner_uid(owner_uid: u32) -> Self {
        Self::with_owner_uid_and_environment_and_display(
            owner_uid,
            Some(current_process_environment_snapshot()),
            Some(current_process_environment_display_snapshot()),
            SubscriptionLimits::default(),
        )
    }

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

    #[cfg(all(any(unix, windows), feature = "web"))]
    pub(crate) fn with_owner_uid_subscription_limits_and_web_settings(
        owner_uid: u32,
        subscription_limits: SubscriptionLimits,
        web_settings: crate::web::WebShareSettings,
    ) -> Self {
        let mut handler = Self::with_owner_uid_and_environment_and_display(
            owner_uid,
            Some(current_process_environment_snapshot()),
            Some(current_process_environment_display_snapshot()),
            subscription_limits,
        );
        handler.web_shares = Arc::new(WebShareRegistry::new(web_settings));
        handler
    }

    #[cfg(test)]
    fn with_owner_uid_and_environment(
        owner_uid: u32,
        environment: Option<HashMap<String, String>>,
        subscription_limits: SubscriptionLimits,
    ) -> Self {
        Self::with_owner_uid_and_environment_and_display(
            owner_uid,
            environment,
            None,
            subscription_limits,
        )
    }

    fn with_owner_uid_and_environment_and_display(
        owner_uid: u32,
        environment: Option<HashMap<String, String>>,
        display_environment: Option<HashMap<String, String>>,
        subscription_limits: SubscriptionLimits,
    ) -> Self {
        let (hook_events, _receiver) = broadcast::channel(HOOK_EVENT_BUFFER);
        let lifecycle_dispatch = Arc::new(BoundedDispatchQueue::new(LIFECYCLE_DISPATCH_BUFFER));
        let mut state = HandlerState::default();
        let task_runtime = tokio::runtime::Handle::try_current().ok();
        #[cfg(unix)]
        if let Some(runtime) = crate::pane_reader_runtime::PaneReaderRuntime::current() {
            state.set_pane_reader_runtime(runtime);
        }
        if let Some(environment) = environment {
            seed_global_environment(&mut state, environment);
        }
        if let Some(environment) = display_environment {
            seed_global_display_environment(&mut state, environment);
        }
        Self {
            state: Arc::new(Mutex::new(state)),
            active_attach: Arc::new(Mutex::new(ActiveAttachState::default())),
            active_attach_epoch: Arc::new(AtomicU64::new(0)),
            active_attach_forwarders: Arc::new(AtomicUsize::new(0)),
            active_control: Arc::new(Mutex::new(ActiveControlState::default())),
            silence_timers: Arc::new(StdMutex::new(HashMap::new())),
            pane_alert_coalescer: Arc::new(StdMutex::new(
                alert_support::PaneAlertCoalescer::default(),
            )),
            pane_alert_dispatch: Arc::new(Mutex::new(())),
            prompt_history: Arc::new(Mutex::new(prompt_support::PromptHistoryStore::default())),
            wait_for: Arc::new(StdMutex::new(WaitForStore::default())),
            hook_events,
            lifecycle_dispatch,
            startup_config_errors: Arc::new(Mutex::new(Vec::new())),
            server_socket_path: Arc::new(StdMutex::new(PathBuf::from("/tmp/rmux-test.sock"))),
            server_access: Arc::new(StdMutex::new(ServerAccessStore::new(owner_uid))),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            shutdown_reason: Arc::new(StdMutex::new(None)),
            shutdown_retry_scheduled: Arc::new(AtomicBool::new(false)),
            active_detached_connections: Arc::new(StdMutex::new(HashSet::new())),
            active_detached_requester_access: Arc::new(StdMutex::new(HashMap::new())),
            active_detached_requests: Arc::new(AtomicUsize::new(0)),
            shutdown_handle: Arc::new(StdMutex::new(None)),
            config_loading_depth: Arc::new(AtomicUsize::new(0)),
            next_connection_id: Arc::new(AtomicU64::new(1)),
            subscriptions: Arc::new(StdMutex::new(OutputSubscriptionState::new(
                subscription_limits,
            ))),
            retained_exited_outputs: Arc::new(StdMutex::new(RetainedExitedPaneOutputs::default())),
            sdk_waits: Arc::new(StdMutex::new(SdkWaitState::default())),
            session_leases: Arc::new(StdMutex::new(SessionLeaseStore::default())),
            session_lease_janitor_started: Arc::new(AtomicBool::new(false)),
            pane_snapshot_coalescers: Arc::new(StdMutex::new(
                PaneSnapshotCoalescerRegistry::with_default_rate(),
            )),
            pane_snapshot_revisions: Arc::new(StdMutex::new(
                PaneSnapshotRevisionRegistry::default(),
            )),
            pane_state_journal: Arc::new(StdMutex::new(PaneStateJournal::with_limits(
                PANE_STATE_JOURNAL_CAPACITY,
                subscription_limits,
            ))),
            pane_state_notify: Arc::new(Notify::new()),
            foreground_watch_started: Arc::new(AtomicBool::new(false)),
            foreground_state_cache: Arc::new(StdMutex::new(HashMap::new())),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: Arc::new(WebShareRegistry::default()),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: Arc::new(Mutex::new(())),
            task_runtime,
            #[cfg(test)]
            cleanup_on_drop: true,
            #[cfg(test)]
            paste_buffer_delete_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            window_lifecycle_mutation_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            window_lifecycle_emit_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            control_notification_delivery_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            silence_timer_apply_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_state_lag_rebase_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_option_journal_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_exit_commit_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            alert_plan_effect_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_alert_apply_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attached_size_selection_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attached_size_apply_pause: Arc::new(StdMutex::new(None)),
        }
    }

    pub(crate) fn downgrade(&self) -> WeakRequestHandler {
        WeakRequestHandler {
            state: Arc::downgrade(&self.state),
            active_attach: Arc::downgrade(&self.active_attach),
            active_attach_epoch: Arc::downgrade(&self.active_attach_epoch),
            active_attach_forwarders: Arc::downgrade(&self.active_attach_forwarders),
            active_control: Arc::downgrade(&self.active_control),
            silence_timers: Arc::downgrade(&self.silence_timers),
            pane_alert_coalescer: Arc::downgrade(&self.pane_alert_coalescer),
            pane_alert_dispatch: Arc::downgrade(&self.pane_alert_dispatch),
            prompt_history: Arc::downgrade(&self.prompt_history),
            wait_for: Arc::downgrade(&self.wait_for),
            hook_events: self.hook_events.clone(),
            lifecycle_dispatch: Arc::downgrade(&self.lifecycle_dispatch),
            startup_config_errors: Arc::downgrade(&self.startup_config_errors),
            server_socket_path: Arc::downgrade(&self.server_socket_path),
            server_access: Arc::downgrade(&self.server_access),
            shutdown_requested: Arc::downgrade(&self.shutdown_requested),
            shutdown_reason: Arc::downgrade(&self.shutdown_reason),
            shutdown_retry_scheduled: Arc::downgrade(&self.shutdown_retry_scheduled),
            active_detached_connections: Arc::downgrade(&self.active_detached_connections),
            active_detached_requester_access: Arc::downgrade(
                &self.active_detached_requester_access,
            ),
            active_detached_requests: Arc::downgrade(&self.active_detached_requests),
            shutdown_handle: Arc::downgrade(&self.shutdown_handle),
            config_loading_depth: Arc::downgrade(&self.config_loading_depth),
            next_connection_id: Arc::downgrade(&self.next_connection_id),
            subscriptions: Arc::downgrade(&self.subscriptions),
            retained_exited_outputs: Arc::downgrade(&self.retained_exited_outputs),
            sdk_waits: Arc::downgrade(&self.sdk_waits),
            session_leases: Arc::downgrade(&self.session_leases),
            session_lease_janitor_started: Arc::downgrade(&self.session_lease_janitor_started),
            pane_snapshot_coalescers: Arc::downgrade(&self.pane_snapshot_coalescers),
            pane_snapshot_revisions: Arc::downgrade(&self.pane_snapshot_revisions),
            pane_state_journal: Arc::downgrade(&self.pane_state_journal),
            pane_state_notify: Arc::downgrade(&self.pane_state_notify),
            foreground_watch_started: Arc::downgrade(&self.foreground_watch_started),
            foreground_state_cache: Arc::downgrade(&self.foreground_state_cache),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: Arc::downgrade(&self.web_shares),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: Arc::downgrade(&self.web_listener_start),
            task_runtime: self.task_runtime.clone(),
            #[cfg(test)]
            paste_buffer_delete_pause: Arc::downgrade(&self.paste_buffer_delete_pause),
        }
    }

    pub(crate) fn allocate_connection_id(&self) -> u64 {
        self.next_connection_id.fetch_add(1, Ordering::Relaxed)
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

    pub(crate) fn access_mode_for_peer(&self, peer: &PeerIdentity) -> Option<AccessMode> {
        self.server_access
            .lock()
            .ok()
            .and_then(|server_access| server_access.mode_for_identity(&peer.user))
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

    #[cfg(test)]
    async fn wait_for_initial_panes_for_test(&self) {
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_panes_ready().await;
    }
}

#[cfg(test)]
#[path = "handler_send_keys_tests/input_capture.rs"]
mod input_capture;

#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "handler_attach_tests.rs"]
mod attach_tests;

#[cfg(test)]
#[path = "handler_window_tests.rs"]
mod window_tests;

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
#[path = "handler_show_tests.rs"]
mod show_tests;

#[cfg(test)]
#[path = "handler_buffer_tests.rs"]
mod buffer_tests;

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
#[path = "handler_winlink_insertion_tests.rs"]
mod winlink_insertion_tests;

#[cfg(test)]
#[path = "handler_pane_alert_race_tests.rs"]
mod pane_alert_race_tests;

#[cfg(test)]
#[path = "handler_clock_mode_tests.rs"]
mod clock_mode_tests;

#[cfg(test)]
#[path = "handler_control_notification_tests.rs"]
mod control_notification_tests;

#[cfg(test)]
#[path = "handler_control_lifecycle_tests.rs"]
mod control_lifecycle_tests;

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
#[path = "handler_pane_family_lifecycle_tests.rs"]
mod pane_family_lifecycle_tests;

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
#[path = "handler_pane_window_metadata_tests.rs"]
mod pane_window_metadata_tests;

#[cfg(test)]
#[path = "handler_pane_pipe_tests.rs"]
mod pane_pipe_tests;

#[cfg(test)]
#[path = "handler_pane_exit_format_tests.rs"]
mod pane_exit_format_tests;

#[cfg(test)]
#[path = "handler_pane_silence_timer_tests.rs"]
mod pane_silence_timer_tests;

#[cfg(test)]
#[path = "handler_pane_state_tests.rs"]
mod pane_state_tests;

#[cfg(test)]
#[path = "handler_pane_state_race_tests.rs"]
mod pane_state_race_tests;

#[cfg(test)]
#[path = "handler_pane_alias_lifecycle_tests.rs"]
mod pane_alias_lifecycle_tests;

#[cfg(test)]
#[path = "handler_linked_pane_kill_tests.rs"]
mod linked_pane_kill_tests;
