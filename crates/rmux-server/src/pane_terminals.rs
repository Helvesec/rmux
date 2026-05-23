use std::collections::{HashMap, HashSet, VecDeque};
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

use crate::pane_io::{PaneAlertCallback, PaneExitCallback, PaneOutputSender};
#[cfg(unix)]
use crate::pane_reader_runtime::PaneReaderRuntime;
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::pane_transcript::SharedPaneTranscript;

#[path = "pane_terminals/lifecycle_state.rs"]
mod lifecycle_state;
#[path = "pane_terminals/pane_access.rs"]
mod pane_access;
#[path = "pane_terminals/pane_lifecycle.rs"]
mod pane_lifecycle;
#[path = "pane_terminals/pane_outputs.rs"]
mod pane_outputs;
#[path = "pane_pipe.rs"]
mod pane_pipe;
#[path = "pane_terminal_store.rs"]
mod pane_terminal_store;
#[path = "pane_terminals/pane_transcripts.rs"]
mod pane_transcripts;
#[path = "pane_terminals/pane_transfer.rs"]
mod pane_transfer;
#[path = "pane_terminals/rollback.rs"]
mod rollback;
#[path = "pane_terminals/session_mutation.rs"]
mod session_mutation;
#[path = "pane_terminals/session_runtime.rs"]
mod session_runtime;
#[path = "pane_terminals/window_links.rs"]
mod window_links;
#[path = "pane_terminals_window.rs"]
mod window_support;

#[cfg(test)]
pub(crate) use lifecycle_state::PaneLifecycleProcessState;
use lifecycle_state::PaneLifecycleSpawn;
pub(crate) use lifecycle_state::PaneLifecycleState;
pub(crate) use pane_outputs::PaneExitMetadata;
use pane_outputs::{AttachedSubmittedLine, PaneOutputSpawn, RemovedPaneOutputs};
use pane_pipe::{ActivePanePipe, PanePipeStore};
use pane_terminal_store::PaneTerminalStore;
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use pane_transcripts::PaneCaptureRequest;
use window_links::{WindowLinkGroup, WindowLinkSlot};

#[derive(Clone)]
pub(crate) struct WindowSpawnOptions<'a> {
    pub(crate) start_directory: Option<&'a Path>,
    pub(crate) command: Option<&'a ProcessCommand>,
    pub(crate) socket_path: &'a Path,
    pub(crate) environment_overrides: Option<&'a [String]>,
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
    pane_output_generations: HashMap<SessionName, HashMap<PaneId, u64>>,
    pane_lifecycle: HashMap<PaneId, PaneLifecycleState>,
    attached_submitted_rows: HashMap<SessionName, HashMap<PaneId, AttachedSubmittedLine>>,
    attached_terminal_pixels: HashMap<SessionName, TerminalPixels>,
    #[cfg(test)]
    pane_input_captures: StdMutex<HashMap<String, Vec<u8>>>,
    dead_panes: HashMap<SessionName, HashMap<PaneId, PaneExitMetadata>>,
    marked_pane: Option<PaneId>,
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

    pub(crate) fn marked_pane_target(&self) -> Option<PaneTarget> {
        let pane_id = self.marked_pane?;
        self.sessions.iter().find_map(|(session_name, session)| {
            let window_index = session.window_index_for_pane_id(pane_id)?;
            let pane_index = session
                .window_at(window_index)?
                .panes()
                .iter()
                .find(|pane| pane.id() == pane_id)
                .map(|pane| pane.index())?;
            Some(PaneTarget::with_window(
                session_name.clone(),
                window_index,
                pane_index,
            ))
        })
    }

    pub(crate) fn pane_is_marked(&self, target: &PaneTarget) -> bool {
        pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
            == self.marked_pane
    }

    pub(crate) fn session_has_marked_pane(&self, session_name: &SessionName) -> bool {
        self.marked_pane_target()
            .is_some_and(|target| target.session_name() == session_name)
    }

    pub(crate) fn window_has_marked_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> bool {
        self.marked_pane_target().is_some_and(|target| {
            target.session_name() == session_name && target.window_index() == window_index
        })
    }

    pub(crate) fn clear_marked_pane(&mut self) {
        self.marked_pane = None;
    }

    pub(crate) fn toggle_marked_pane(&mut self, target: &PaneTarget) -> Result<bool, RmuxError> {
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        if self.marked_pane == Some(pane_id) {
            self.marked_pane = None;
            Ok(false)
        } else {
            self.marked_pane = Some(pane_id);
            Ok(true)
        }
    }

    pub(crate) fn pipe_pane(
        &mut self,
        target: PaneTarget,
        command: Option<String>,
        read_from_pipe: bool,
        write_to_pipe: bool,
        once: bool,
    ) -> Result<rmux_proto::PipePaneResponse, RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let pane_index = target.pane_index();
        let pane_id = pane_id_for_target(&self.sessions, &session_name, window_index, pane_index)?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);

        if once && self.pipes.contains(&runtime_session_name, pane_id) {
            return Ok(rmux_proto::PipePaneResponse { target });
        }

        if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, pane_id) {
            pipe.stop();
        }

        let Some(command) = command.filter(|command| !command.is_empty()) else {
            return Ok(rmux_proto::PipePaneResponse { target });
        };

        let pane_master =
            self.clone_pane_master_if_alive(&session_name, window_index, pane_index)?;
        let pane_output = self.pane_output_for_target(&session_name, window_index, pane_index)?;
        let profile = self
            .terminals
            .pane_profile(&runtime_session_name, pane_id, window_index, pane_index)?
            .clone();
        let pipe = ActivePanePipe::spawn(
            &profile,
            pane_output,
            pane_master,
            &command,
            read_from_pipe,
            write_to_pipe,
        )?;
        if let Some(previous) = self.pipes.insert(&runtime_session_name, pane_id, pipe) {
            previous.stop();
        }

        Ok(rmux_proto::PipePaneResponse { target })
    }

    fn remove_session_pipes(
        &mut self,
        session_name: &SessionName,
    ) -> HashMap<PaneId, ActivePanePipe> {
        self.pipes.remove_session(session_name)
    }

    fn remove_pane_pipe(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<ActivePanePipe> {
        self.pipes.remove(session_name, pane_id)
    }

    fn clear_marked_pane_if_id(&mut self, pane_id: PaneId) {
        if self.marked_pane == Some(pane_id) {
            self.marked_pane = None;
        }
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
        let session = self
            .sessions
            .session_mut(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let should_update = match session.window_at(window_index) {
            Some(window) => window.automatic_rename() && window.name().is_none(),
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
        session.rename_window(window_index, window_name)?;
        self.mark_auto_named_window(session_name, window_index);
        self.synchronize_linked_window_from_slot(session_name, window_index)?;
        self.synchronize_session_group_from(session_name)?;
        Ok(())
    }
}

fn pane_terminal_geometry_for_session(
    session: &Session,
    options: &OptionStore,
    geometry: PaneGeometry,
) -> PaneGeometry {
    let content_rows = session_content_rows(session, options);
    let y = geometry.y().min(content_rows);
    let rows = geometry.rows().min(content_rows.saturating_sub(y));
    PaneGeometry::new(geometry.x(), y, geometry.cols(), rows)
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

    use super::HandlerState;
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
                std::path::Path::new("/tmp/rmux-test.sock"),
                None,
                None,
                None,
                None,
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
}
