use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use rmux_core::events::{PaneSnapshotCoalescerRegistry, SubscriptionLimits};
#[cfg(test)]
use rmux_proto::{OptionName, ScopeSelector, SetOptionMode};
use tokio::sync::{broadcast, Mutex, Notify};

#[cfg(all(any(unix, windows), feature = "web"))]
use super::WebShareRegistry;
use super::{
    alert_support, background_tasks, current_owner_uid,
    current_process_environment_display_snapshot, current_process_environment_snapshot,
    lifecycle_producer_tasks, post_commit_sequencer, prompt_support,
    seed_global_display_environment, seed_global_environment, shell_processes, shutdown_support,
    ActiveAttachState, ActiveControlState, ClipboardQueryState, HandlerState,
    LifecycleDispatchOutbox, OutputSubscriptionState, PaneSnapshotRevisionRegistry,
    PaneStateJournal, RequestHandler, RetainedExitedPaneOutputs, SdkWaitState, ServerAccessStore,
    SessionLeaseStore, WaitForStore, HOOK_EVENT_BUFFER, LIFECYCLE_DISPATCH_BUFFER,
    PANE_MODE_POST_COMMIT_LIMIT, PANE_STATE_JOURNAL_CAPACITY,
};

impl Default for RequestHandler {
    fn default() -> Self {
        Self::with_owner_uid(current_owner_uid())
    }
}

impl RequestHandler {
    /// Test handler whose panes cannot write anything the test did not ask
    /// for.
    ///
    /// On Unix, every pane spawned without an explicit command runs the quiet
    /// stdin-discard child under `/bin/sh`, instead of the host's login shell.
    /// A login shell prints its banner and prompt whenever it is scheduled, and
    /// what it prints depends on the host (`$SHELL`, its startup files, its
    /// prompt theme). A test that captures a pane or waits for a silence alert
    /// would otherwise read that output, or see it re-arm the silence timers of
    /// the whole window family.
    ///
    /// A test that is about the shell itself (its name, its argv, the input it
    /// echoes and runs, an unset `default-shell`) asks for it with
    /// [`Self::with_host_shell_for_test`].
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        let handler = Self::with_host_shell_for_test();
        // Windows keeps its default shell here until native evidence covers
        // the whole suite; fixtures there pin explicitly.
        if cfg!(unix) {
            handler.pin_quiet_pane_defaults_before_first_use();
        }
        handler
    }

    /// Test handler whose panes run the host's default shell, as the daemon
    /// does.
    #[cfg(test)]
    pub(crate) fn with_host_shell_for_test() -> Self {
        Self::with_owner_uid_and_environment(
            current_owner_uid(),
            None,
            SubscriptionLimits::default(),
        )
    }

    /// Pins the same global options as `pin_quiet_default_command_for_test`,
    /// on a handler nobody else holds yet.
    #[cfg(test)]
    fn pin_quiet_pane_defaults_before_first_use(&self) {
        let mut state = self
            .state
            .try_lock()
            .expect("a handler under construction has no other state holder");
        for (option, value) in [
            (OptionName::DefaultShell, "/bin/sh".to_owned()),
            (
                OptionName::DefaultCommand,
                crate::test_shell::stdin_discard_command(),
            ),
        ] {
            state
                .options
                .set(ScopeSelector::Global, option, value, SetOptionMode::Replace)
                .expect("quiet pane default is a valid global option");
        }
    }

    #[cfg(test)]
    pub(crate) fn with_lifecycle_dispatch_capacity_for_test(capacity: usize) -> Self {
        let mut handler = Self::new();
        handler.lifecycle_dispatch = Arc::new(LifecycleDispatchOutbox::new(capacity));
        handler
    }

    pub(crate) fn with_owner_uid(owner_uid: u32) -> Self {
        Self::with_owner_uid_and_environment_and_display(
            owner_uid,
            Some(current_process_environment_snapshot()),
            Some(current_process_environment_display_snapshot()),
            SubscriptionLimits::default(),
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
    pub(in crate::handler) fn with_owner_uid_and_environment(
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

    pub(super) fn with_owner_uid_and_environment_and_display(
        owner_uid: u32,
        environment: Option<HashMap<String, String>>,
        display_environment: Option<HashMap<String, String>>,
        subscription_limits: SubscriptionLimits,
    ) -> Self {
        let (hook_events, _receiver) = broadcast::channel(HOOK_EVENT_BUFFER);
        let lifecycle_dispatch = Arc::new(LifecycleDispatchOutbox::new(LIFECYCLE_DISPATCH_BUFFER));
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
            clipboard_queries: Arc::new(StdMutex::new(ClipboardQueryState::default())),
            active_attach_epoch: Arc::new(AtomicU64::new(0)),
            active_attach_forwarders: Arc::new(AtomicUsize::new(0)),
            client_creation_sequence: Arc::new(AtomicU64::new(0)),
            client_activity_sequence: Arc::new(AtomicU64::new(0)),
            client_size_sequence: Arc::new(AtomicU64::new(0)),
            active_control: Arc::new(Mutex::new(ActiveControlState::default())),
            silence_timers: Arc::new(StdMutex::new(HashMap::new())),
            pane_alert_coalescer: Arc::new(StdMutex::new(
                alert_support::PaneAlertCoalescer::default(),
            )),
            pane_renames: Arc::new(crate::pane_rename_queue::PaneRenameQueue::new()),
            pane_alert_dispatch: Arc::new(Mutex::new(())),
            lifecycle_producers: Arc::new(
                lifecycle_producer_tasks::LifecycleProducerRegistry::new(),
            ),
            prompt_history: Arc::new(Mutex::new(prompt_support::PromptHistoryStore::default())),
            wait_for: Arc::new(StdMutex::new(WaitForStore::default())),
            hook_events,
            lifecycle_dispatch,
            startup_config_errors: Arc::new(Mutex::new(Vec::new())),
            server_socket_path: Arc::new(StdMutex::new(PathBuf::from("/tmp/rmux-test.sock"))),
            server_access: Arc::new(StdMutex::new(ServerAccessStore::new(owner_uid))),
            server_access_mutation: Arc::new(Mutex::new(())),
            #[cfg(unix)]
            unix_socket_access: Arc::new(StdMutex::new(None)),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            shutdown_reason: Arc::new(StdMutex::new(None)),
            shutdown_retry_state: Arc::new(StdMutex::new(
                shutdown_support::ShutdownRetryState::new(),
            )),
            active_detached_connections: Arc::new(StdMutex::new(HashSet::new())),
            active_detached_requester_access: Arc::new(StdMutex::new(HashMap::new())),
            active_detached_requests: Arc::new(AtomicUsize::new(0)),
            normal_request_admission: Arc::new(shutdown_support::NormalRequestAdmission::new()),
            background_tasks: Arc::new(background_tasks::BackgroundTaskRegistry::new()),
            shell_processes: Arc::new(shell_processes::ShellProcessRegistry::new()),
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
            pane_mode_transaction: Arc::new(Mutex::new(())),
            pane_mode_post_commit: Arc::new(post_commit_sequencer::PostCommitSequencer::new(
                PANE_MODE_POST_COMMIT_LIMIT,
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
            attached_mouse_timer_pause: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            live_input_work: Arc::default(),
        }
    }
}
