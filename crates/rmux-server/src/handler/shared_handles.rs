#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, Weak};

use rmux_core::events::PaneSnapshotCoalescerRegistry;
use rmux_proto::{RmuxError, WindowTarget};
use tokio::sync::{broadcast, Mutex, Notify};

#[cfg(unix)]
use super::UnixSocketAccessController;
#[cfg(all(any(unix, windows), feature = "web"))]
use super::WebShareRegistry;
use super::{
    alert_support, background_tasks, lifecycle_producer_tasks, post_commit_sequencer,
    prompt_support, shell_processes, shutdown_support, ActiveAttachState, ActiveControlState,
    ClipboardQueryState, DetachedRequesterAccess, HandlerState, LifecycleDispatchItem,
    LifecycleDispatchOutbox, OutputSubscriptionState, PaneSnapshotRevisionRegistry,
    PaneStateJournal, PendingShutdownReason, QueuedLifecycleEvent, RequestHandler,
    RetainedExitedPaneOutputs, SdkWaitState, ServerAccessStore, SessionLeaseStore, ShutdownHandle,
    WaitForStore,
};
#[cfg(test)]
use super::{pane_support, PaneExitCommitPause, PasteBufferDeletePause, SilenceTimerApplyPause};

impl Clone for RequestHandler {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            active_attach: self.active_attach.clone(),
            clipboard_queries: self.clipboard_queries.clone(),
            active_attach_epoch: self.active_attach_epoch.clone(),
            active_attach_forwarders: self.active_attach_forwarders.clone(),
            client_creation_sequence: self.client_creation_sequence.clone(),
            client_activity_sequence: self.client_activity_sequence.clone(),
            client_size_sequence: self.client_size_sequence.clone(),
            active_control: self.active_control.clone(),
            silence_timers: self.silence_timers.clone(),
            pane_alert_coalescer: self.pane_alert_coalescer.clone(),
            pane_renames: self.pane_renames.clone(),
            pane_alert_dispatch: self.pane_alert_dispatch.clone(),
            lifecycle_producers: self.lifecycle_producers.clone(),
            prompt_history: self.prompt_history.clone(),
            wait_for: self.wait_for.clone(),
            hook_events: self.hook_events.clone(),
            lifecycle_dispatch: self.lifecycle_dispatch.clone(),
            startup_config_errors: self.startup_config_errors.clone(),
            server_socket_path: self.server_socket_path.clone(),
            server_access: self.server_access.clone(),
            server_access_mutation: self.server_access_mutation.clone(),
            #[cfg(unix)]
            unix_socket_access: self.unix_socket_access.clone(),
            shutdown_requested: self.shutdown_requested.clone(),
            shutdown_reason: self.shutdown_reason.clone(),
            shutdown_retry_state: self.shutdown_retry_state.clone(),
            source_relays: self.source_relays.clone(),
            active_detached_connections: self.active_detached_connections.clone(),
            active_detached_requester_access: self.active_detached_requester_access.clone(),
            active_detached_requests: self.active_detached_requests.clone(),
            normal_request_admission: self.normal_request_admission.clone(),
            background_tasks: self.background_tasks.clone(),
            shell_processes: self.shell_processes.clone(),
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
            pane_mode_transaction: self.pane_mode_transaction.clone(),
            pane_mode_post_commit: self.pane_mode_post_commit.clone(),
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
            initial_pane_tasks: self.initial_pane_tasks.clone(),
            #[cfg(test)]
            surface_admission_pause: self.surface_admission_pause.clone(),
            #[cfg(test)]
            surface_admission_materializations: self.surface_admission_materializations.clone(),
            #[cfg(test)]
            surface_poll_materializations: self.surface_poll_materializations.clone(),
            #[cfg(test)]
            alert_plan_effect_pause: self.alert_plan_effect_pause.clone(),
            #[cfg(test)]
            pane_alert_apply_pause: self.pane_alert_apply_pause.clone(),
            #[cfg(test)]
            pane_exit_rename_pause: self.pane_exit_rename_pause.clone(),
            #[cfg(test)]
            attached_size_selection_pause: self.attached_size_selection_pause.clone(),
            #[cfg(test)]
            attached_size_apply_pause: self.attached_size_apply_pause.clone(),
            #[cfg(test)]
            attach_registration_activity_pause: self.attach_registration_activity_pause.clone(),
            #[cfg(test)]
            attached_mouse_timer_pause: self.attached_mouse_timer_pause.clone(),
            #[cfg(test)]
            live_input_work: self.live_input_work.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct WeakRequestHandler {
    state: Weak<Mutex<HandlerState>>,
    active_attach: Weak<Mutex<ActiveAttachState>>,
    clipboard_queries: Weak<StdMutex<ClipboardQueryState>>,
    active_attach_epoch: Weak<AtomicU64>,
    active_attach_forwarders: Weak<AtomicUsize>,
    client_creation_sequence: Weak<AtomicU64>,
    client_activity_sequence: Weak<AtomicU64>,
    client_size_sequence: Weak<AtomicU64>,
    active_control: Weak<Mutex<ActiveControlState>>,
    silence_timers: Weak<StdMutex<HashMap<WindowTarget, alert_support::SilenceTimerState>>>,
    pane_alert_coalescer: Weak<StdMutex<alert_support::PaneAlertCoalescer>>,
    pane_renames: Weak<crate::pane_rename_queue::PaneRenameQueue>,
    pane_alert_dispatch: Weak<Mutex<()>>,
    lifecycle_producers: Weak<lifecycle_producer_tasks::LifecycleProducerRegistry>,
    prompt_history: Weak<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Weak<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
    lifecycle_dispatch: Weak<LifecycleDispatchOutbox<LifecycleDispatchItem>>,
    startup_config_errors: Weak<Mutex<Vec<RmuxError>>>,
    server_socket_path: Weak<StdMutex<PathBuf>>,
    server_access: Weak<StdMutex<ServerAccessStore>>,
    server_access_mutation: Weak<Mutex<()>>,
    #[cfg(unix)]
    unix_socket_access: Weak<StdMutex<Option<UnixSocketAccessController>>>,
    shutdown_requested: Weak<AtomicBool>,
    shutdown_reason: Weak<StdMutex<Option<PendingShutdownReason>>>,
    shutdown_retry_state: Weak<StdMutex<shutdown_support::ShutdownRetryState>>,
    source_relays: Weak<crate::listener::source_relay::PendingRelays>,
    active_detached_connections: Weak<StdMutex<HashSet<u64>>>,
    active_detached_requester_access: Weak<StdMutex<HashMap<u32, DetachedRequesterAccess>>>,
    active_detached_requests: Weak<AtomicUsize>,
    normal_request_admission: Weak<shutdown_support::NormalRequestAdmission>,
    background_tasks: Weak<background_tasks::BackgroundTaskRegistry>,
    shell_processes: Weak<shell_processes::ShellProcessRegistry>,
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
    pane_mode_transaction: Weak<Mutex<()>>,
    pane_mode_post_commit: Weak<post_commit_sequencer::PostCommitSequencer>,
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_shares: Weak<WebShareRegistry>,
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_listener_start: Weak<Mutex<()>>,
    task_runtime: Option<tokio::runtime::Handle>,
    #[cfg(test)]
    paste_buffer_delete_pause: Weak<StdMutex<Option<Arc<PasteBufferDeletePause>>>>,
    #[cfg(test)]
    silence_timer_apply_pause: Weak<StdMutex<Option<Arc<SilenceTimerApplyPause>>>>,
    #[cfg(test)]
    pane_exit_commit_pause: Weak<StdMutex<Option<Arc<PaneExitCommitPause>>>>,
    #[cfg(test)]
    attached_mouse_timer_pause: Weak<StdMutex<Option<Arc<pane_support::AttachedMouseTimerPause>>>>,
    #[cfg(test)]
    live_input_work: Weak<pane_support::LiveInputWorkProbe>,
}

impl WeakRequestHandler {
    pub(crate) fn upgrade(&self) -> Option<RequestHandler> {
        Some(RequestHandler {
            state: self.state.upgrade()?,
            active_attach: self.active_attach.upgrade()?,
            clipboard_queries: self.clipboard_queries.upgrade()?,
            active_attach_epoch: self.active_attach_epoch.upgrade()?,
            active_attach_forwarders: self.active_attach_forwarders.upgrade()?,
            client_creation_sequence: self.client_creation_sequence.upgrade()?,
            client_activity_sequence: self.client_activity_sequence.upgrade()?,
            client_size_sequence: self.client_size_sequence.upgrade()?,
            active_control: self.active_control.upgrade()?,
            silence_timers: self.silence_timers.upgrade()?,
            pane_alert_coalescer: self.pane_alert_coalescer.upgrade()?,
            pane_renames: self.pane_renames.upgrade()?,
            pane_alert_dispatch: self.pane_alert_dispatch.upgrade()?,
            lifecycle_producers: self.lifecycle_producers.upgrade()?,
            prompt_history: self.prompt_history.upgrade()?,
            wait_for: self.wait_for.upgrade()?,
            hook_events: self.hook_events.clone(),
            lifecycle_dispatch: self.lifecycle_dispatch.upgrade()?,
            startup_config_errors: self.startup_config_errors.upgrade()?,
            server_socket_path: self.server_socket_path.upgrade()?,
            server_access: self.server_access.upgrade()?,
            server_access_mutation: self.server_access_mutation.upgrade()?,
            #[cfg(unix)]
            unix_socket_access: self.unix_socket_access.upgrade()?,
            shutdown_requested: self.shutdown_requested.upgrade()?,
            shutdown_reason: self.shutdown_reason.upgrade()?,
            shutdown_retry_state: self.shutdown_retry_state.upgrade()?,
            source_relays: self.source_relays.upgrade()?,
            active_detached_connections: self.active_detached_connections.upgrade()?,
            active_detached_requester_access: self.active_detached_requester_access.upgrade()?,
            active_detached_requests: self.active_detached_requests.upgrade()?,
            normal_request_admission: self.normal_request_admission.upgrade()?,
            background_tasks: self.background_tasks.upgrade()?,
            shell_processes: self.shell_processes.upgrade()?,
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
            pane_mode_transaction: self.pane_mode_transaction.upgrade()?,
            pane_mode_post_commit: self.pane_mode_post_commit.upgrade()?,
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
            silence_timer_apply_pause: self.silence_timer_apply_pause.upgrade()?,
            #[cfg(test)]
            pane_state_lag_rebase_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_option_journal_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_exit_commit_pause: self.pane_exit_commit_pause.upgrade()?,
            #[cfg(test)]
            initial_pane_tasks: Arc::new(StdMutex::new(Vec::new())),
            #[cfg(test)]
            surface_admission_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            surface_admission_materializations: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            surface_poll_materializations: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            alert_plan_effect_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_alert_apply_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            pane_exit_rename_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attached_size_selection_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attached_size_apply_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attach_registration_activity_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            attached_mouse_timer_pause: self.attached_mouse_timer_pause.upgrade()?,
            #[cfg(test)]
            live_input_work: self.live_input_work.upgrade()?,
        })
    }
}

impl RequestHandler {
    pub(crate) fn downgrade(&self) -> WeakRequestHandler {
        WeakRequestHandler {
            state: Arc::downgrade(&self.state),
            active_attach: Arc::downgrade(&self.active_attach),
            clipboard_queries: Arc::downgrade(&self.clipboard_queries),
            active_attach_epoch: Arc::downgrade(&self.active_attach_epoch),
            active_attach_forwarders: Arc::downgrade(&self.active_attach_forwarders),
            client_creation_sequence: Arc::downgrade(&self.client_creation_sequence),
            client_activity_sequence: Arc::downgrade(&self.client_activity_sequence),
            client_size_sequence: Arc::downgrade(&self.client_size_sequence),
            active_control: Arc::downgrade(&self.active_control),
            silence_timers: Arc::downgrade(&self.silence_timers),
            pane_alert_coalescer: Arc::downgrade(&self.pane_alert_coalescer),
            pane_renames: Arc::downgrade(&self.pane_renames),
            pane_alert_dispatch: Arc::downgrade(&self.pane_alert_dispatch),
            lifecycle_producers: Arc::downgrade(&self.lifecycle_producers),
            prompt_history: Arc::downgrade(&self.prompt_history),
            wait_for: Arc::downgrade(&self.wait_for),
            hook_events: self.hook_events.clone(),
            lifecycle_dispatch: Arc::downgrade(&self.lifecycle_dispatch),
            startup_config_errors: Arc::downgrade(&self.startup_config_errors),
            server_socket_path: Arc::downgrade(&self.server_socket_path),
            server_access: Arc::downgrade(&self.server_access),
            server_access_mutation: Arc::downgrade(&self.server_access_mutation),
            #[cfg(unix)]
            unix_socket_access: Arc::downgrade(&self.unix_socket_access),
            shutdown_requested: Arc::downgrade(&self.shutdown_requested),
            shutdown_reason: Arc::downgrade(&self.shutdown_reason),
            shutdown_retry_state: Arc::downgrade(&self.shutdown_retry_state),
            source_relays: Arc::downgrade(&self.source_relays),
            active_detached_connections: Arc::downgrade(&self.active_detached_connections),
            active_detached_requester_access: Arc::downgrade(
                &self.active_detached_requester_access,
            ),
            active_detached_requests: Arc::downgrade(&self.active_detached_requests),
            normal_request_admission: Arc::downgrade(&self.normal_request_admission),
            background_tasks: Arc::downgrade(&self.background_tasks),
            shell_processes: Arc::downgrade(&self.shell_processes),
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
            pane_mode_transaction: Arc::downgrade(&self.pane_mode_transaction),
            pane_mode_post_commit: Arc::downgrade(&self.pane_mode_post_commit),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: Arc::downgrade(&self.web_shares),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: Arc::downgrade(&self.web_listener_start),
            task_runtime: self.task_runtime.clone(),
            #[cfg(test)]
            paste_buffer_delete_pause: Arc::downgrade(&self.paste_buffer_delete_pause),
            #[cfg(test)]
            silence_timer_apply_pause: Arc::downgrade(&self.silence_timer_apply_pause),
            #[cfg(test)]
            pane_exit_commit_pause: Arc::downgrade(&self.pane_exit_commit_pause),
            #[cfg(test)]
            attached_mouse_timer_pause: Arc::downgrade(&self.attached_mouse_timer_pause),
            #[cfg(test)]
            live_input_work: Arc::downgrade(&self.live_input_work),
        }
    }
}
