use std::fmt;
use std::path::{Path, PathBuf};

use rmux_core::{PaneGeometry, PaneId};
use rmux_proto::{ProcessCommand, SessionId, SessionName, TerminalSize, WindowId};

use super::{HandlerState, PaneExitMetadata};

#[derive(Clone, PartialEq, Eq, Default)]
pub(in crate::pane_terminals) struct PrivatePaneEnvironment(Vec<String>);

impl PrivatePaneEnvironment {
    fn new(environment: Option<&[String]>) -> Self {
        Self(environment.unwrap_or_default().to_vec())
    }

    #[cfg(test)]
    pub(crate) fn as_slice(&self) -> &[String] {
        &self.0
    }

    fn to_vec(&self) -> Vec<String> {
        self.0.clone()
    }
}

impl fmt::Debug for PrivatePaneEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivatePaneEnvironment")
            .field("entry_count", &self.0.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PaneLifecycleProcessState {
    #[default]
    Unknown,
    #[cfg(windows)]
    Starting,
    Running {
        pid: Option<u32>,
    },
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneLifecycleExitState {
    pub(crate) status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) time: Option<i64>,
}

impl From<PaneExitMetadata> for PaneLifecycleExitState {
    fn from(metadata: PaneExitMetadata) -> Self {
        Self {
            status: metadata.status,
            signal: metadata.signal,
            time: metadata.time,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PaneLifecycleState {
    pub(crate) session_id: SessionId,
    pub(crate) window_id: WindowId,
    pub(crate) pane_id: PaneId,
    command: Option<Vec<String>>,
    process_command: Option<ProcessCommand>,
    working_directory: Option<PathBuf>,
    respawn_shell: PathBuf,
    private_environment: PrivatePaneEnvironment,
    respawn_environment: PrivatePaneEnvironment,
    tags: Vec<String>,
    dimensions: TerminalSize,
    pub(crate) process: PaneLifecycleProcessState,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) output_sequence: u64,
    pub(crate) exit_state: Option<PaneLifecycleExitState>,
}

impl fmt::Debug for PaneLifecycleState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaneLifecycleState")
            .field("session_id", &self.session_id)
            .field("window_id", &self.window_id)
            .field("pane_id", &self.pane_id)
            .field("command", &self.command)
            .field("working_directory", &self.working_directory)
            .field("private_environment", &self.private_environment)
            .field("tags", &self.tags)
            .field("dimensions", &self.dimensions)
            .field("process", &self.process)
            .field("generation", &self.generation)
            .field("revision", &self.revision)
            .field("output_sequence", &self.output_sequence)
            .field("exit_state", &self.exit_state)
            .finish()
    }
}

impl PaneLifecycleState {
    #[cfg(test)]
    pub(crate) fn command(&self) -> Option<&[String]> {
        self.command.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn process_command(&self) -> Option<&ProcessCommand> {
        self.process_command.as_ref()
    }

    pub(crate) fn encoded_command(&self) -> Option<String> {
        self.command.as_deref().map(format_command_field)
    }

    pub(crate) fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn respawn_shell(&self) -> &Path {
        &self.respawn_shell
    }

    #[cfg(test)]
    pub(crate) fn tags(&self) -> &[String] {
        &self.tags
    }

    #[cfg(test)]
    pub(crate) const fn dimensions(&self) -> TerminalSize {
        self.dimensions
    }

    #[cfg(test)]
    pub(crate) fn private_environment(&self) -> &[String] {
        self.private_environment.as_slice()
    }

    #[cfg(test)]
    pub(crate) fn respawn_environment(&self) -> &[String] {
        self.respawn_environment.as_slice()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PaneRespawnProvenance {
    pub(crate) process_command: Option<ProcessCommand>,
    pub(crate) working_directory: Option<PathBuf>,
    pub(crate) shell: PathBuf,
    pub(crate) private_environment: Vec<String>,
}

pub(in crate::pane_terminals) struct PaneLifecycleSpawn {
    pub(in crate::pane_terminals) session_id: SessionId,
    pub(in crate::pane_terminals) window_id: WindowId,
    pub(in crate::pane_terminals) pane_id: PaneId,
    pub(in crate::pane_terminals) process_command: Option<ProcessCommand>,
    pub(in crate::pane_terminals) working_directory: Option<PathBuf>,
    pub(in crate::pane_terminals) respawn_shell: PathBuf,
    pub(in crate::pane_terminals) private_environment: Option<Vec<String>>,
    /// Overrides the private environment retained for later command-less
    /// respawns. `None` records the current private environment as the
    /// baseline; respawn paths pass `Some` to keep the original baseline when
    /// `-e` applies only to the replacement process.
    pub(in crate::pane_terminals) respawn_environment: Option<Vec<String>>,
    pub(in crate::pane_terminals) dimensions: TerminalSize,
    pub(in crate::pane_terminals) pid: Option<u32>,
}

impl HandlerState {
    #[cfg(windows)]
    pub(crate) fn pane_start_command_for_id(&self, pane_id: PaneId) -> Option<&[String]> {
        self.pane_lifecycle.get(&pane_id)?.command.as_deref()
    }

    #[cfg(windows)]
    pub(in crate::pane_terminals) fn record_pane_lifecycle_starting(
        &mut self,
        spawn: PaneLifecycleSpawn,
    ) -> u64 {
        self.record_pane_lifecycle_with_process(spawn, PaneLifecycleProcessState::Starting)
    }

    pub(in crate::pane_terminals) fn record_pane_lifecycle_spawn(
        &mut self,
        spawn: PaneLifecycleSpawn,
    ) {
        let pid = spawn.pid;
        self.record_pane_lifecycle_with_process(spawn, PaneLifecycleProcessState::Running { pid });
    }

    fn record_pane_lifecycle_with_process(
        &mut self,
        spawn: PaneLifecycleSpawn,
        process: PaneLifecycleProcessState,
    ) -> u64 {
        let PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id,
            process_command,
            working_directory,
            respawn_shell,
            private_environment,
            respawn_environment,
            dimensions,
            pid: _,
        } = spawn;
        let command = process_command
            .as_ref()
            .map(ProcessCommand::display_command);
        let respawn_environment =
            respawn_environment.unwrap_or_else(|| private_environment.clone().unwrap_or_default());
        let previous = self.pane_lifecycle.remove(&pane_id);
        let generation = previous
            .as_ref()
            .map_or(1, |state| state.generation.saturating_add(1));
        let revision = previous
            .as_ref()
            .map_or(1, |state| state.revision.saturating_add(1));
        let output_sequence = previous.as_ref().map_or(0, |state| state.output_sequence);

        self.pane_lifecycle.insert(
            pane_id,
            PaneLifecycleState {
                session_id,
                window_id,
                pane_id,
                command,
                process_command,
                working_directory,
                respawn_shell,
                private_environment: PrivatePaneEnvironment::new(private_environment.as_deref()),
                respawn_environment: PrivatePaneEnvironment::new(Some(&respawn_environment)),
                tags: Vec::new(),
                dimensions,
                process,
                generation,
                revision,
                output_sequence,
                exit_state: None,
            },
        );
        generation
    }

    #[cfg(windows)]
    pub(in crate::pane_terminals) fn mark_pane_lifecycle_running(
        &mut self,
        pane_id: PaneId,
        pid: Option<u32>,
    ) {
        let Some(state) = self.pane_lifecycle.get_mut(&pane_id) else {
            return;
        };
        if state.process == (PaneLifecycleProcessState::Running { pid }) {
            return;
        }
        state.process = PaneLifecycleProcessState::Running { pid };
        state.revision = state.revision.saturating_add(1);
    }

    pub(in crate::pane_terminals) fn update_pane_lifecycle_output_sequence(
        &mut self,
        pane_id: PaneId,
        output_sequence: u64,
    ) {
        let Some(state) = self.pane_lifecycle.get_mut(&pane_id) else {
            return;
        };
        if state.output_sequence != output_sequence {
            state.output_sequence = output_sequence;
            state.revision = state.revision.saturating_add(1);
        }
    }

    pub(in crate::pane_terminals) fn mark_pane_lifecycle_exited(
        &mut self,
        pane_id: PaneId,
        metadata: PaneExitMetadata,
    ) {
        let Some(state) = self.pane_lifecycle.get_mut(&pane_id) else {
            return;
        };
        let exit_state = metadata.into();
        if state.process == PaneLifecycleProcessState::Exited
            && state.exit_state == Some(exit_state)
        {
            return;
        }
        state.process = PaneLifecycleProcessState::Exited;
        state.exit_state = Some(exit_state);
        state.generation = state.generation.saturating_add(1);
        state.revision = state.revision.saturating_add(1);
    }

    pub(in crate::pane_terminals) fn remove_pane_lifecycle(&mut self, pane_id: PaneId) {
        let _ = self.pane_lifecycle.remove(&pane_id);
    }

    pub(in crate::pane_terminals) fn remove_pane_lifecycles<'a>(
        &mut self,
        pane_ids: impl IntoIterator<Item = &'a PaneId>,
    ) {
        for pane_id in pane_ids {
            self.remove_pane_lifecycle(*pane_id);
        }
    }

    pub(in crate::pane_terminals) fn sync_pane_lifecycle_dimensions_for_session(
        &mut self,
        session_name: &SessionName,
    ) {
        let Some(session) = self.sessions.session(session_name) else {
            return;
        };
        let session_id = session.id();
        let updates = session
            .windows()
            .values()
            .flat_map(|window| {
                window.panes().iter().map(move |pane| {
                    (
                        pane.id(),
                        session_id,
                        window.id(),
                        terminal_size_from_geometry(pane.geometry()),
                    )
                })
            })
            .collect::<Vec<_>>();

        for (pane_id, session_id, window_id, dimensions) in updates {
            let Some(state) = self.pane_lifecycle.get_mut(&pane_id) else {
                continue;
            };
            let changed = state.session_id != session_id
                || state.window_id != window_id
                || state.dimensions != dimensions;
            if changed {
                state.session_id = session_id;
                state.window_id = window_id;
                state.dimensions = dimensions;
                state.revision = state.revision.saturating_add(1);
            }
        }
    }

    pub(crate) fn pane_lifecycle(&self, pane_id: PaneId) -> Option<&PaneLifecycleState> {
        self.pane_lifecycle.get(&pane_id)
    }

    pub(crate) fn pane_respawn_provenance(&self, pane_id: PaneId) -> Option<PaneRespawnProvenance> {
        let lifecycle = self.pane_lifecycle.get(&pane_id)?;
        Some(PaneRespawnProvenance {
            process_command: lifecycle.process_command.clone(),
            working_directory: lifecycle.working_directory.clone(),
            shell: lifecycle.respawn_shell.clone(),
            private_environment: lifecycle.respawn_environment.to_vec(),
        })
    }
}

pub(crate) fn terminal_size_from_geometry(geometry: PaneGeometry) -> TerminalSize {
    TerminalSize {
        cols: geometry.cols().max(1),
        rows: geometry.rows().max(1),
    }
}

fn format_command_field(command: &[String]) -> String {
    if let [command] = command {
        return quote_single_shell_command(command);
    }
    encode_command_field(command)
}

fn quote_single_shell_command(command: &str) -> String {
    if !command.contains(char::is_whitespace) && !command.contains('"') && !command.contains('\\') {
        return command.to_owned();
    }
    format!("\"{}\"", command.replace('\\', "\\\\").replace('"', "\\\""))
}

fn encode_command_field(command: &[String]) -> String {
    command
        .iter()
        .map(|argument| percent_encode(argument.as_bytes()))
        .collect::<Vec<_>>()
        .join("\x1f")
}

fn percent_encode(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for byte in bytes {
        match *byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'/'
            | b':'
            | b' '
            | b'='
            | b'+' => encoded.push(char::from(*byte)),
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{encode_command_field, terminal_size_from_geometry};
    use rmux_core::PaneGeometry;
    use rmux_proto::TerminalSize;

    #[test]
    fn command_encoding_removes_record_separators_and_newlines() {
        let encoded = encode_command_field(&[
            "printf".to_owned(),
            "alpha\tbeta\nsecret\x1fgamma%".to_owned(),
        ]);

        assert_eq!(encoded, "printf\u{1f}alpha%09beta%0Asecret%1Fgamma%25");
        assert!(!encoded.contains('\t'));
        assert!(!encoded.contains('\n'));
    }

    #[test]
    fn terminal_size_from_geometry_never_sends_zero_sized_pty() {
        assert_eq!(
            terminal_size_from_geometry(PaneGeometry::new(0, 0, 0, 0)),
            TerminalSize { cols: 1, rows: 1 }
        );
        assert_eq!(
            terminal_size_from_geometry(PaneGeometry::new(0, 23, 80, 0)),
            TerminalSize { cols: 80, rows: 1 }
        );
    }
}
