use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::Path;
#[cfg(test)]
use std::sync::Mutex as StdMutex;

use rmux_core::{
    BufferStore, EnvironmentStore, HookStore, KeyBindingStore, OptionStore, PaneGeometry, PaneId,
    Session, SessionStore,
};
use rmux_proto::{
    KillPaneResponse, KillWindowResponse, OptionName, PaneTarget, ProcessCommand, RmuxError,
    SessionName, TerminalPixels, WindowTarget,
};

#[cfg(unix)]
use crate::pane_io::PaneOutputReaderTask;
use crate::pane_io::{PaneAlertCallback, PaneExitCallback, PaneOutputSender};
#[cfg(unix)]
use crate::pane_reader_runtime::PaneReaderRuntime;
use crate::pane_transcript::SharedPaneTranscript;
use crate::pane_visible_geometry::visible_pane_content_geometry;

#[path = "pane_terminals/lifecycle_state.rs"]
mod lifecycle_state;
#[path = "pane_terminals/marked_pane.rs"]
mod marked_pane;
#[path = "pane_terminals/pane_access.rs"]
mod pane_access;
#[path = "pane_terminals/pane_lifecycle.rs"]
mod pane_lifecycle;
#[path = "pane_terminals/pane_outputs.rs"]
mod pane_outputs;
#[path = "pane_pipe.rs"]
mod pane_pipe;
#[cfg(feature = "web")]
#[path = "pane_terminals/pane_scrollback.rs"]
mod pane_scrollback;
#[path = "pane_terminal_store.rs"]
mod pane_terminal_store;
#[path = "pane_terminals/pane_transcripts.rs"]
mod pane_transcripts;
#[path = "pane_terminals/pane_transfer.rs"]
mod pane_transfer;
#[path = "pane_terminals/pipes.rs"]
mod pipes;
#[path = "pane_terminals/rollback.rs"]
mod rollback;
#[path = "pane_terminals/session_mutation.rs"]
mod session_mutation;
#[path = "pane_terminals/session_runtime.rs"]
mod session_runtime;
#[path = "pane_terminals/window_indices.rs"]
mod window_indices;
#[path = "pane_terminals/window_links.rs"]
mod window_links;
#[path = "pane_terminals_window.rs"]
mod window_support;

#[cfg(test)]
pub(crate) use lifecycle_state::PaneLifecycleProcessState;
use lifecycle_state::PaneLifecycleSpawn;
pub(crate) use lifecycle_state::PaneLifecycleState;
use marked_pane::MarkedPane;
pub(crate) use pane_outputs::PaneExitMetadata;
use pane_outputs::{AttachedSubmittedLine, PaneOutputSpawn, RemovedPaneOutputs};
use pane_pipe::PanePipeStore;
use pane_terminal_store::PaneTerminalStore;
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use pane_transcripts::PaneCaptureRequest;
use window_links::{WindowLinkGroup, WindowLinkSlot};

#[derive(Clone)]
pub(crate) struct WindowSpawnOptions<'a> {
    pub(crate) start_directory: Option<&'a Path>,
    pub(crate) command: Option<&'a ProcessCommand>,
    pub(crate) socket_path: &'a Path,
    pub(crate) spawn_environment: Option<&'a HashMap<String, String>>,
    pub(crate) environment_overrides: Option<&'a [String]>,
    pub(crate) pane_alert_callback: Option<PaneAlertCallback>,
    pub(crate) pane_exit_callback: Option<PaneExitCallback>,
}

pub(crate) struct InitialPaneSpawnOptions<'a> {
    pub(crate) socket_path: &'a Path,
    pub(crate) spawn_environment: Option<&'a HashMap<String, String>>,
    pub(crate) raw_spawn_environment: Option<&'a [(OsString, OsString)]>,
    pub(crate) environment_overrides: Option<&'a [String]>,
    pub(crate) command: Option<&'a ProcessCommand>,
    pub(crate) pane_alert_callback: Option<PaneAlertCallback>,
    pub(crate) pane_exit_callback: Option<PaneExitCallback>,
}

pub(crate) struct NewWindowOptions<'a> {
    pub(crate) name: Option<String>,
    pub(crate) detached: bool,
    pub(crate) spawn: WindowSpawnOptions<'a>,
}

pub(crate) struct RespawnWindowOptions<'a> {
    pub(crate) kill: bool,
    pub(crate) spawn: WindowSpawnOptions<'a>,
}

#[derive(Debug, Default)]
pub(crate) struct HandlerState {
    pub(crate) sessions: SessionStore,
    pub(crate) options: OptionStore,
    pub(crate) environment: EnvironmentStore,
    pub(crate) hooks: HookStore,
    pub(crate) buffers: BufferStore,
    pub(crate) key_bindings: KeyBindingStore,
    pub(crate) message_log: VecDeque<MessageEntry>,
    next_message_number: u64,
    terminals: PaneTerminalStore,
    transcripts: HashMap<SessionName, HashMap<PaneId, SharedPaneTranscript>>,
    pane_outputs: HashMap<SessionName, HashMap<PaneId, PaneOutputSender>>,
    #[cfg(unix)]
    pane_output_readers: HashMap<SessionName, HashMap<PaneId, PaneOutputReaderTask>>,
    pane_output_generations: HashMap<SessionName, HashMap<PaneId, u64>>,
    pane_lifecycle: HashMap<PaneId, PaneLifecycleState>,
    attached_submitted_rows: HashMap<SessionName, HashMap<PaneId, AttachedSubmittedLine>>,
    attached_terminal_pixels: HashMap<SessionName, TerminalPixels>,
    input_disabled_panes: HashSet<PaneId>,
    #[cfg(test)]
    pane_input_captures: StdMutex<HashMap<String, Vec<u8>>>,
    dead_panes: HashMap<SessionName, HashMap<PaneId, PaneExitMetadata>>,
    marked_pane: Option<MarkedPane>,
    pipes: PanePipeStore,
    auto_named_windows: HashSet<(SessionName, u32)>,
    window_link_groups: HashMap<u64, WindowLinkGroup>,
    window_link_slots: HashMap<WindowLinkSlot, u64>,
    next_window_link_group_id: u64,
    #[cfg(unix)]
    pane_reader_runtime: Option<PaneReaderRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageEntry {
    pub(crate) msg_time: i64,
    pub(crate) msg_num: u64,
    pub(crate) msg: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KilledPaneHookContext {
    pub(crate) target: PaneTarget,
    pub(crate) pane_id: u32,
    pub(crate) window_id: u32,
    pub(crate) window_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KilledPaneResult {
    pub(crate) response: KillPaneResponse,
    pub(crate) hook_context: KilledPaneHookContext,
    pub(crate) session_destroyed: bool,
    pub(crate) removed_session_id: Option<u32>,
    pub(crate) removed_pane_ids: Vec<PaneId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemovedWindowHookContext {
    pub(crate) target: WindowTarget,
    pub(crate) window_id: u32,
    pub(crate) window_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KilledWindowResult {
    pub(crate) response: KillWindowResponse,
    pub(crate) removed_windows: Vec<RemovedWindowHookContext>,
    pub(crate) removed_pane_ids: Vec<PaneId>,
}

impl HandlerState {
    #[cfg(unix)]
    pub(crate) fn set_pane_reader_runtime(&mut self, runtime: PaneReaderRuntime) {
        self.pane_reader_runtime = Some(runtime);
    }

    #[cfg(unix)]
    pub(in crate::pane_terminals) fn pane_reader_runtime(
        &self,
    ) -> Result<PaneReaderRuntime, RmuxError> {
        let runtime = self.pane_reader_runtime.clone();
        #[cfg(test)]
        let runtime = runtime.or_else(PaneReaderRuntime::current);

        runtime.ok_or_else(|| {
            RmuxError::Server(
                "cannot spawn Unix pane output reader without the server Tokio runtime".to_owned(),
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn shutdown_terminals_for_test(&mut self) {
        let mut runtime_sessions = self
            .sessions
            .iter()
            .map(|(session_name, _)| self.runtime_session_name(session_name))
            .collect::<Vec<_>>();
        runtime_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        runtime_sessions.dedup();

        for session_name in runtime_sessions {
            for pipe in self.remove_session_pipes(&session_name).into_values() {
                pipe.stop();
            }
            self.remove_session_pane_outputs(&session_name);
            let _ = self.terminals.remove_session(&session_name);
        }
        self.auto_named_windows.clear();
        self.attached_submitted_rows.clear();
        self.attached_terminal_pixels.clear();
        self.dead_panes.clear();
        self.pane_lifecycle.clear();
    }

    pub(crate) fn set_attached_terminal_pixels(
        &mut self,
        session_name: &SessionName,
        pixels: Option<TerminalPixels>,
    ) {
        match pixels {
            Some(pixels) => {
                self.attached_terminal_pixels
                    .insert(session_name.clone(), pixels);
            }
            None => {
                self.attached_terminal_pixels.remove(session_name);
            }
        }
    }

    pub(crate) fn add_message(&mut self, message: impl Into<String>) {
        let message = message.into();
        let msg_num = self.next_message_number;
        self.next_message_number = self.next_message_number.saturating_add(1);
        self.message_log.push_back(MessageEntry {
            msg_time: chrono::Local::now().timestamp(),
            msg_num,
            msg: message,
        });

        self.trim_message_log();
    }

    pub(crate) fn trim_message_log(&mut self) {
        let limit = self.message_limit();
        while self.message_log.len() > limit {
            let _ = self.message_log.pop_front();
        }
    }

    #[cfg(unix)]
    pub(crate) fn continue_stopped_panes(&mut self) {
        self.terminals.continue_stopped_panes();
    }

    fn message_limit(&self) -> usize {
        self.options
            .resolve(None, OptionName::MessageLimit)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1000)
    }

    fn apply_automatic_window_name(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        automatic_window_name: Option<String>,
    ) -> Result<(), RmuxError> {
        let Some(window_name) = automatic_window_name else {
            return Ok(());
        };
        let tracked = self.tracks_auto_named_window(session_name, window_index);
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let should_update = match session.window_at(window_index) {
            Some(window) => {
                crate::automatic_rename::window_allows_automatic_rename(
                    &self.options,
                    session_name,
                    window_index,
                    window,
                    tracked,
                ) && window.name().is_none()
            }
            None => {
                return Err(RmuxError::invalid_target(
                    format!("{session_name}:{window_index}"),
                    "window index does not exist in session",
                ))
            }
        };
        if !should_update {
            return Ok(());
        }
        self.sessions
            .session_mut(session_name)
            .expect("existing session must accept automatic rename update")
            .rename_window(window_index, window_name)?;
        self.mark_auto_named_window(session_name, window_index);
        self.synchronize_linked_window_from_slot(session_name, window_index)?;
        self.synchronize_session_group_from(session_name)?;
        Ok(())
    }
}

fn pane_terminal_geometry_for_session(
    session: &Session,
    options: &OptionStore,
    window_index: u32,
    geometry: PaneGeometry,
) -> PaneGeometry {
    let content_rows = session_content_rows(session, options);
    visible_pane_content_geometry(
        options,
        session.name(),
        window_index,
        geometry,
        content_rows,
    )
}

fn session_content_rows(session: &Session, options: &OptionStore) -> u16 {
    let size = session.window().size();
    if size.cols == 0 || size.rows == 0 {
        return size.rows;
    }

    if session.last_attached_at().is_none() {
        return size.rows;
    }

    if matches!(
        options.resolve(Some(session.name()), OptionName::Status),
        Some("off")
    ) {
        size.rows
    } else {
        size.rows.saturating_sub(1)
    }
}

pub(crate) fn session_not_found(session_name: &SessionName) -> RmuxError {
    RmuxError::SessionNotFound(session_name.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{HandlerState, InitialPaneSpawnOptions};
    use rmux_proto::{
        HookLifecycle, HookName, OptionName, PaneTarget, RmuxError, ScopeSelector, SessionName,
        SetOptionMode, TerminalSize, WindowTarget,
    };

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    #[tokio::test]
    async fn rename_session_rolls_back_previous_store_migrations_on_runtime_state_error() {
        let mut state = HandlerState::default();
        let alpha = session_name("alpha");
        let gamma = session_name("gamma");

        state
            .sessions
            .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("session create succeeds");
        state
            .insert_initial_session_terminal(
                &alpha,
                InitialPaneSpawnOptions {
                    socket_path: std::path::Path::new("/tmp/rmux-test.sock"),
                    spawn_environment: None,
                    raw_spawn_environment: None,
                    environment_overrides: None,
                    command: None,
                    pane_alert_callback: None,
                    pane_exit_callback: None,
                },
            )
            .expect("initial terminals exist");
        state
            .options
            .set(
                ScopeSelector::Session(alpha.clone()),
                OptionName::Status,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("session option set succeeds");
        state
            .options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
                OptionName::MainPaneWidth,
                "90".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("window option set succeeds");
        state
            .options
            .set(
                ScopeSelector::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                OptionName::WindowStyle,
                "default,bold".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane option set succeeds");
        state.environment.set(
            ScopeSelector::Session(alpha.clone()),
            "TERM".to_owned(),
            "screen".to_owned(),
        );
        state
            .hooks
            .set(
                ScopeSelector::Session(alpha.clone()),
                HookName::AfterSendKeys,
                "true".to_owned(),
                HookLifecycle::Persistent,
            )
            .expect("hook set succeeds");
        state.pane_outputs.insert(gamma.clone(), HashMap::new());

        let error = state
            .rename_session(&alpha, &gamma)
            .expect_err("conflicting runtime state rejects rename");

        assert_eq!(
            error,
            RmuxError::Server("pane output channels already exist for session gamma".to_owned())
        );
        assert!(state.sessions.contains_session(&alpha));
        assert!(!state.sessions.contains_session(&gamma));
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("original session still exists")
                .name(),
            &alpha
        );
        assert_eq!(
            state.options.resolve(Some(&alpha), OptionName::Status),
            Some("off")
        );
        assert_eq!(
            state
                .options
                .resolve_for_window(&alpha, 0, OptionName::MainPaneWidth),
            Some("90")
        );
        assert_eq!(
            state
                .options
                .resolve_for_pane(&alpha, 0, 0, OptionName::WindowStyle),
            Some("default,bold")
        );
        assert_eq!(
            state.environment.session_value(&alpha, "TERM"),
            Some("screen")
        );
        assert_eq!(
            state.hooks.session_command(&alpha, HookName::AfterSendKeys),
            Some("true")
        );
        assert!(state.contains_session_terminals(&alpha));
        assert!(state.transcripts.contains_key(&alpha));
        assert!(state.pane_outputs.contains_key(&alpha));
        assert!(state.pane_outputs.contains_key(&gamma));
    }

    #[tokio::test]
    async fn rename_session_migrates_runtime_output_generations() {
        let mut state = HandlerState::default();
        let alpha = session_name("alpha");
        let beta = session_name("beta");

        state
            .sessions
            .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("session create succeeds");
        state
            .insert_initial_session_terminal(
                &alpha,
                InitialPaneSpawnOptions {
                    socket_path: std::path::Path::new("/tmp/rmux-test.sock"),
                    spawn_environment: None,
                    raw_spawn_environment: None,
                    environment_overrides: None,
                    command: None,
                    pane_alert_callback: None,
                    pane_exit_callback: None,
                },
            )
            .expect("initial terminals exist");

        let pane_id = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.active_pane())
            .map(|pane| pane.id())
            .expect("initial pane exists");
        let generation = state.pane_output_generation(&alpha, pane_id);
        assert!(generation > 0);
        #[cfg(unix)]
        assert!(
            state
                .pane_output_readers
                .get(&alpha)
                .is_some_and(|readers| readers.contains_key(&pane_id)),
            "initial pane reader task must be owned by the runtime session"
        );

        state
            .rename_session(&alpha, &beta)
            .expect("rename succeeds");

        assert!(!state.pane_output_generations.contains_key(&alpha));
        #[cfg(unix)]
        {
            assert!(!state.pane_output_readers.contains_key(&alpha));
            assert!(
                state
                    .pane_output_readers
                    .get(&beta)
                    .is_some_and(|readers| readers.contains_key(&pane_id)),
                "rename must re-key pane reader ownership for cleanup"
            );
        }
        assert_eq!(
            state.pane_output_generation(&beta, pane_id),
            generation,
            "rename must preserve pane output generations for stale reader callbacks"
        );
        state.shutdown_terminals_for_test();
    }
}
