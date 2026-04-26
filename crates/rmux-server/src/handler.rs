use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::{Arc, Weak};

use rmux_ipc::PeerIdentity;
use rmux_proto::{KillServerResponse, Response, RmuxError, TerminalSize, WindowTarget};
use tokio::sync::{broadcast, Mutex};

use crate::daemon::ShutdownHandle;
#[path = "handler_alerts.rs"]
mod alert_support;
#[path = "handler_attach.rs"]
pub(crate) mod attach_support;
#[path = "handler_buffer.rs"]
mod buffer_support;
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
#[path = "handler_dispatch.rs"]
mod dispatch_support;
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
#[path = "handler_session.rs"]
mod session_support;
#[path = "handler_targets.rs"]
mod target_support;
#[path = "handler_window.rs"]
mod window_support;
use crate::pane_terminals::HandlerState;
use crate::server_access::{current_owner_uid, user_name_for_uid, AccessMode, ServerAccessStore};
use crate::wait_for::WaitForStore;
use attach_support::{ActiveAttachState, ClientFlags};
pub(in crate::handler) use client_runtime_support::{
    attached_client_matches_target, client_environment_snapshot, clipboard_query_sequence,
    command_output_from_lines, effective_client_terminal_context, normalize_target_client,
    parse_client_flags, parse_session_sort_order, session_selection_prefers_live_process,
    sort_list_clients, switch_target_selector_count, update_environment_from_client,
    ListClientSnapshot, SessionSortOrder, LIST_CLIENTS_TEMPLATE,
};
use client_runtime_support::{current_process_environment_snapshot, seed_global_environment};
#[cfg(test)]
pub(in crate::handler) use client_runtime_support::{
    format_attached_client_flags, format_control_client_flags,
};
use control_support::ActiveControlState;
#[cfg(test)]
pub(in crate::handler) use lifecycle_support::after_hook_format_values;
pub(in crate::handler) use lifecycle_support::prepare_lifecycle_event;
pub(crate) use lifecycle_support::QueuedLifecycleEvent;
use option_support::option_value_u32;
pub(in crate::handler) use target_support::{
    active_session_target, active_window_target, fallback_current_target,
    resolve_existing_session_target, resolve_session_lookup, target_for_request_response,
    target_for_scope_selector, target_to_scope, SessionLookup,
};

/// Default detached session size used when `new-session` omits `-x` and `-y`.
///
/// RMUX currently chooses the conventional 80x24 baseline until client-side
/// terminal discovery is wired in later steps.
pub const DEFAULT_SESSION_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const HOOK_EVENT_BUFFER: usize = 256;

#[derive(Debug)]
pub(crate) struct RequestHandler {
    state: Arc<Mutex<HandlerState>>,
    active_attach: Arc<Mutex<ActiveAttachState>>,
    active_control: Arc<Mutex<ActiveControlState>>,
    silence_timers: Arc<StdMutex<HashMap<WindowTarget, alert_support::SilenceTimerState>>>,
    prompt_history: Arc<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Arc<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
    startup_config_errors: Arc<Mutex<Vec<RmuxError>>>,
    server_socket_path: Arc<StdMutex<PathBuf>>,
    server_access: Arc<StdMutex<ServerAccessStore>>,
    shutdown_requested: Arc<AtomicBool>,
    shutdown_handle: Arc<StdMutex<Option<ShutdownHandle>>>,
    config_loading_depth: Arc<AtomicUsize>,
    #[cfg(test)]
    cleanup_on_drop: bool,
    #[cfg(test)]
    paste_buffer_delete_pause: Arc<StdMutex<Option<Arc<PasteBufferDeletePause>>>>,
}

impl Clone for RequestHandler {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            active_attach: self.active_attach.clone(),
            active_control: self.active_control.clone(),
            silence_timers: self.silence_timers.clone(),
            prompt_history: self.prompt_history.clone(),
            wait_for: self.wait_for.clone(),
            hook_events: self.hook_events.clone(),
            startup_config_errors: self.startup_config_errors.clone(),
            server_socket_path: self.server_socket_path.clone(),
            server_access: self.server_access.clone(),
            shutdown_requested: self.shutdown_requested.clone(),
            shutdown_handle: self.shutdown_handle.clone(),
            config_loading_depth: self.config_loading_depth.clone(),
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
    prompt_history: Weak<Mutex<prompt_support::PromptHistoryStore>>,
    wait_for: Weak<StdMutex<WaitForStore>>,
    hook_events: broadcast::Sender<QueuedLifecycleEvent>,
    startup_config_errors: Weak<Mutex<Vec<RmuxError>>>,
    server_socket_path: Weak<StdMutex<PathBuf>>,
    server_access: Weak<StdMutex<ServerAccessStore>>,
    shutdown_requested: Weak<AtomicBool>,
    shutdown_handle: Weak<StdMutex<Option<ShutdownHandle>>>,
    config_loading_depth: Weak<AtomicUsize>,
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
            prompt_history: self.prompt_history.upgrade()?,
            wait_for: self.wait_for.upgrade()?,
            hook_events: self.hook_events.clone(),
            startup_config_errors: self.startup_config_errors.upgrade()?,
            server_socket_path: self.server_socket_path.upgrade()?,
            server_access: self.server_access.upgrade()?,
            shutdown_requested: self.shutdown_requested.upgrade()?,
            shutdown_handle: self.shutdown_handle.upgrade()?,
            config_loading_depth: self.config_loading_depth.upgrade()?,
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
        Self::with_owner_uid_and_environment(current_owner_uid(), None)
    }

    pub(crate) fn with_owner_uid(owner_uid: u32) -> Self {
        Self::with_owner_uid_and_environment(
            owner_uid,
            Some(current_process_environment_snapshot()),
        )
    }

    fn with_owner_uid_and_environment(
        owner_uid: u32,
        environment: Option<HashMap<String, String>>,
    ) -> Self {
        let (hook_events, _receiver) = broadcast::channel(HOOK_EVENT_BUFFER);
        let mut state = HandlerState::default();
        if let Some(environment) = environment {
            seed_global_environment(&mut state, environment);
        }
        Self {
            state: Arc::new(Mutex::new(state)),
            active_attach: Arc::new(Mutex::new(ActiveAttachState::default())),
            active_control: Arc::new(Mutex::new(ActiveControlState::default())),
            silence_timers: Arc::new(StdMutex::new(HashMap::new())),
            prompt_history: Arc::new(Mutex::new(prompt_support::PromptHistoryStore::default())),
            wait_for: Arc::new(StdMutex::new(WaitForStore::default())),
            hook_events,
            startup_config_errors: Arc::new(Mutex::new(Vec::new())),
            server_socket_path: Arc::new(StdMutex::new(PathBuf::from("/tmp/rmux-test.sock"))),
            server_access: Arc::new(StdMutex::new(ServerAccessStore::new(owner_uid))),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            shutdown_handle: Arc::new(StdMutex::new(None)),
            config_loading_depth: Arc::new(AtomicUsize::new(0)),
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
            prompt_history: Arc::downgrade(&self.prompt_history),
            wait_for: Arc::downgrade(&self.wait_for),
            hook_events: self.hook_events.clone(),
            startup_config_errors: Arc::downgrade(&self.startup_config_errors),
            server_socket_path: Arc::downgrade(&self.server_socket_path),
            server_access: Arc::downgrade(&self.server_access),
            shutdown_requested: Arc::downgrade(&self.shutdown_requested),
            shutdown_handle: Arc::downgrade(&self.shutdown_handle),
            config_loading_depth: Arc::downgrade(&self.config_loading_depth),
            #[cfg(test)]
            paste_buffer_delete_pause: Arc::downgrade(&self.paste_buffer_delete_pause),
        }
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

    pub(crate) fn config_loading_active(&self) -> bool {
        self.config_loading_depth.load(Ordering::Relaxed) != 0
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

    pub(crate) fn request_shutdown_if_pending(&self) -> bool {
        if !self.shutdown_requested.swap(false, Ordering::SeqCst) {
            return false;
        }
        if let Some(handle) = self
            .shutdown_handle
            .lock()
            .expect("shutdown handle mutex must not be poisoned")
            .clone()
        {
            handle.request_shutdown();
        }
        true
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

    async fn take_startup_config_error(&self) -> Option<RmuxError> {
        let mut errors = self.startup_config_errors.lock().await;
        if errors.is_empty() {
            return None;
        }

        match errors.len() {
            1 => Some(errors.pop().expect("one startup config error")),
            _ => Some(RmuxError::Server(
                errors
                    .drain(..)
                    .map(|error| match error {
                        RmuxError::Server(message) => message,
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
        }
    }
}

impl RequestHandler {
    async fn handle_kill_server(&self) -> Response {
        self.shutdown_requested.store(true, Ordering::SeqCst);
        Response::KillServer(KillServerResponse)
    }
}

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
#[path = "handler_scripting_tests.rs"]
mod scripting_tests;

#[cfg(test)]
#[path = "handler_prompt_tests.rs"]
mod prompt_tests;

#[cfg(test)]
#[path = "handler_pane_command_tests.rs"]
mod pane_command_tests;
