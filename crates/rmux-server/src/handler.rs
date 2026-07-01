use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, Weak};

use rmux_core::events::{PaneSnapshotCoalescerRegistry, SubscriptionLimits};
use rmux_ipc::PeerIdentity;
use rmux_proto::{RmuxError, TerminalSize, WindowTarget};
use tokio::sync::{broadcast, Mutex};

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
pub(crate) use shutdown_support::DetachedRequestGuard;
#[path = "handler_subscriptions.rs"]
mod subscription_support;
#[path = "handler_target_actions.rs"]
mod target_action_support;
#[path = "handler_targets.rs"]
mod target_support;
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
pub(crate) use control_support::ControlRegistration;
use exited_output_support::RetainedExitedPaneOutputs;
#[cfg(test)]
pub(in crate::handler) use lifecycle_support::after_hook_format_values;
pub(in crate::handler) use lifecycle_support::prepare_lifecycle_event;
pub(crate) use lifecycle_support::QueuedLifecycleEvent;
use option_support::option_value_u32;
use pane_support::PaneSnapshotRevisionRegistry;
use session_lease_support::SessionLeaseStore;
use subscription_support::OutputSubscriptionState;
pub(in crate::handler) use target_support::{
    active_session_target, active_window_target, fallback_current_target,
    resolve_existing_session_target, resolve_session_lookup, target_for_request_response,
    target_for_scope_selector, target_to_scope, with_visible_pane_bases, SessionLookup,
};
use wait_support::SdkWaitState;

/// Default detached session size used when `new-session` omits `-x` and `-y`.
///
/// RMUX currently chooses the conventional 80x24 baseline until client-side
/// terminal discovery is wired in later steps.
pub const DEFAULT_SESSION_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const HOOK_EVENT_BUFFER: usize = 256;

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
    active_control: Arc<Mutex<ActiveControlState>>,
    silence_timers: Arc<StdMutex<HashMap<WindowTarget, alert_support::SilenceTimerState>>>,
    pane_alert_coalescer: Arc<StdMutex<alert_support::PaneAlertCoalescer>>,
    prompt_history: Arc<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Arc<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
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
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_shares: Arc<WebShareRegistry>,
    #[cfg(all(any(unix, windows), feature = "web"))]
    web_listener_start: Arc<Mutex<()>>,
    task_runtime: Option<tokio::runtime::Handle>,
    #[cfg(test)]
    cleanup_on_drop: bool,
    #[cfg(test)]
    paste_buffer_delete_pause: Arc<StdMutex<Option<Arc<PasteBufferDeletePause>>>>,
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
            active_control: self.active_control.clone(),
            silence_timers: self.silence_timers.clone(),
            pane_alert_coalescer: self.pane_alert_coalescer.clone(),
            prompt_history: self.prompt_history.clone(),
            wait_for: self.wait_for.clone(),
            hook_events: self.hook_events.clone(),
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
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: self.web_shares.clone(),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: self.web_listener_start.clone(),
            task_runtime: self.task_runtime.clone(),
            #[cfg(test)]
            cleanup_on_drop: false,
            #[cfg(test)]
            paste_buffer_delete_pause: self.paste_buffer_delete_pause.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct WeakRequestHandler {
    state: Weak<Mutex<HandlerState>>,
    active_attach: Weak<Mutex<ActiveAttachState>>,
    active_control: Weak<Mutex<ActiveControlState>>,
    silence_timers: Weak<StdMutex<HashMap<WindowTarget, alert_support::SilenceTimerState>>>,
    pane_alert_coalescer: Weak<StdMutex<alert_support::PaneAlertCoalescer>>,
    prompt_history: Weak<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Weak<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
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
            active_control: self.active_control.upgrade()?,
            silence_timers: self.silence_timers.upgrade()?,
            pane_alert_coalescer: self.pane_alert_coalescer.upgrade()?,
            prompt_history: self.prompt_history.upgrade()?,
            wait_for: self.wait_for.upgrade()?,
            hook_events: self.hook_events.clone(),
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
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: self.web_shares.upgrade()?,
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: self.web_listener_start.upgrade()?,
            task_runtime: self.task_runtime.clone(),
            #[cfg(test)]
            cleanup_on_drop: false,
            #[cfg(test)]
            paste_buffer_delete_pause: self.paste_buffer_delete_pause.upgrade()?,
        })
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PasteBufferDeletePause {
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
            active_control: Arc::new(Mutex::new(ActiveControlState::default())),
            silence_timers: Arc::new(StdMutex::new(HashMap::new())),
            pane_alert_coalescer: Arc::new(StdMutex::new(
                alert_support::PaneAlertCoalescer::default(),
            )),
            prompt_history: Arc::new(Mutex::new(prompt_support::PromptHistoryStore::default())),
            wait_for: Arc::new(StdMutex::new(WaitForStore::default())),
            hook_events,
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
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_shares: Arc::new(WebShareRegistry::default()),
            #[cfg(all(any(unix, windows), feature = "web"))]
            web_listener_start: Arc::new(Mutex::new(())),
            task_runtime,
            #[cfg(test)]
            cleanup_on_drop: true,
            #[cfg(test)]
            paste_buffer_delete_pause: Arc::new(StdMutex::new(None)),
        }
    }

    pub(crate) fn downgrade(&self) -> WeakRequestHandler {
        WeakRequestHandler {
            state: Arc::downgrade(&self.state),
            active_attach: Arc::downgrade(&self.active_attach),
            active_control: Arc::downgrade(&self.active_control),
            silence_timers: Arc::downgrade(&self.silence_timers),
            pane_alert_coalescer: Arc::downgrade(&self.pane_alert_coalescer),
            prompt_history: Arc::downgrade(&self.prompt_history),
            wait_for: Arc::downgrade(&self.wait_for),
            hook_events: self.hook_events.clone(),
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
#[path = "handler_pane_pipe_tests.rs"]
mod pane_pipe_tests;

#[cfg(test)]
#[path = "handler_pane_exit_format_tests.rs"]
mod pane_exit_format_tests;
