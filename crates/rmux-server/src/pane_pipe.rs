use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::Stdio;
#[cfg(windows)]
use std::sync::Mutex as StdMutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};

use rmux_core::events::OutputCursorItem;
use rmux_core::PaneId;
use rmux_os::process_tree::{ConsoleWindowBehavior, ProcessTreeChild};
use rmux_proto::{RmuxError, SessionName};
use rmux_pty::PtyMaster;
use tokio::sync::{mpsc, watch};

#[path = "pane_pipe/process_group.rs"]
mod process_group;
use process_group::{wait_for_pipe_child, PipeChildProcessGroup};

use crate::pane_io::{PaneOutputReceiver, PaneOutputSender};
use crate::terminal::TerminalProfile;

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct PipeProcessGroupProbe(Arc<PipeChildProcessGroup>);

#[cfg(test)]
impl PipeProcessGroupProbe {
    pub(crate) fn is_armed(&self) -> bool {
        self.0.is_armed_for_test()
    }

    pub(crate) fn termination_count(&self) -> usize {
        self.0.termination_count_for_test()
    }

    pub(crate) fn child_wait_pending(&self) -> bool {
        self.0.child_wait_pending_for_test()
    }
}

#[derive(Default)]
pub(crate) struct PanePipeStore {
    sessions: HashMap<SessionName, HashMap<PaneId, ActivePanePipe>>,
}

impl std::fmt::Debug for PanePipeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanePipeStore")
            .field("sessions", &self.sessions.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl PanePipeStore {
    pub(crate) fn contains(&self, session_name: &SessionName, pane_id: PaneId) -> bool {
        self.sessions
            .get(session_name)
            .is_some_and(|panes| panes.contains_key(&pane_id))
    }

    #[cfg(test)]
    pub(crate) fn process_group_probe(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<PipeProcessGroupProbe> {
        self.sessions
            .get(session_name)?
            .get(&pane_id)
            .map(ActivePanePipe::process_group_probe)
    }

    pub(crate) fn insert(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        pipe: ActivePanePipe,
    ) -> Option<ActivePanePipe> {
        self.sessions
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, pipe)
    }

    pub(crate) fn remove(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<ActivePanePipe> {
        self.sessions
            .get_mut(session_name)
            .and_then(|panes| panes.remove(&pane_id))
    }

    pub(crate) fn remove_session(
        &mut self,
        session_name: &SessionName,
    ) -> HashMap<PaneId, ActivePanePipe> {
        self.sessions.remove(session_name).unwrap_or_default()
    }

    pub(crate) fn rename_session(
        &mut self,
        session_name: &SessionName,
        new_name: &SessionName,
    ) -> Result<(), RmuxError> {
        if !self.sessions.contains_key(session_name) {
            return Ok(());
        }
        if self.sessions.contains_key(new_name) {
            return Err(RmuxError::Server(format!(
                "pane pipes already exist for session {new_name}"
            )));
        }

        let mut sessions = std::mem::take(&mut self.sessions);
        let panes = sessions
            .remove(session_name)
            .expect("prevalidated pane pipes must exist");
        let replaced = sessions.insert(new_name.clone(), panes);
        debug_assert!(replaced.is_none());
        self.sessions = sessions;
        Ok(())
    }

    pub(crate) fn move_between_sessions(
        &mut self,
        source_session: &SessionName,
        destination_session: &SessionName,
        pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        if source_session == destination_session || pane_ids.is_empty() {
            return Ok(());
        }

        let removed = self.remove_selected(source_session, pane_ids);
        if let Err(error) =
            self.ensure_destination_accepts(destination_session, removed.keys().copied())
        {
            self.sessions
                .entry(source_session.clone())
                .or_default()
                .extend(removed);
            return Err(error);
        }
        self.sessions
            .entry(destination_session.clone())
            .or_default()
            .extend(removed);
        Ok(())
    }

    pub(crate) fn swap_between_sessions(
        &mut self,
        source_session: &SessionName,
        source_pane_ids: &[PaneId],
        destination_session: &SessionName,
        destination_pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        if source_session == destination_session {
            return Ok(());
        }

        let removed_source = self.remove_selected(source_session, source_pane_ids);
        let removed_destination = self.remove_selected(destination_session, destination_pane_ids);

        if let Err(error) =
            self.ensure_destination_accepts(source_session, removed_destination.keys().copied())
        {
            self.sessions
                .entry(source_session.clone())
                .or_default()
                .extend(removed_source);
            self.sessions
                .entry(destination_session.clone())
                .or_default()
                .extend(removed_destination);
            return Err(error);
        }
        if let Err(error) =
            self.ensure_destination_accepts(destination_session, removed_source.keys().copied())
        {
            self.sessions
                .entry(source_session.clone())
                .or_default()
                .extend(removed_source);
            self.sessions
                .entry(destination_session.clone())
                .or_default()
                .extend(removed_destination);
            return Err(error);
        }

        self.sessions
            .entry(source_session.clone())
            .or_default()
            .extend(removed_destination);
        self.sessions
            .entry(destination_session.clone())
            .or_default()
            .extend(removed_source);
        Ok(())
    }

    fn remove_selected(
        &mut self,
        session_name: &SessionName,
        pane_ids: &[PaneId],
    ) -> HashMap<PaneId, ActivePanePipe> {
        let session = self.sessions.entry(session_name.clone()).or_default();
        let mut removed = HashMap::new();
        for pane_id in pane_ids {
            if let Some(pipe) = session.remove(pane_id) {
                removed.insert(*pane_id, pipe);
            }
        }
        removed
    }

    fn ensure_destination_accepts<I>(
        &self,
        session_name: &SessionName,
        pane_ids: I,
    ) -> Result<(), RmuxError>
    where
        I: IntoIterator<Item = PaneId>,
    {
        let session = self.sessions.get(session_name);
        for pane_id in pane_ids {
            if session.is_some_and(|pipes| pipes.contains_key(&pane_id)) {
                return Err(RmuxError::Server(format!(
                    "pane pipe already exists for pane id {} in session {}",
                    pane_id.as_u32(),
                    session_name
                )));
            }
        }
        Ok(())
    }
}

pub(crate) struct ActivePanePipe {
    stop_tx: watch::Sender<bool>,
    stop_flag: Arc<AtomicBool>,
    process_group: Arc<PipeChildProcessGroup>,
    #[cfg(windows)]
    output_abort: Arc<StdMutex<Option<tokio::task::AbortHandle>>>,
}

impl std::fmt::Debug for ActivePanePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivePanePipe").finish_non_exhaustive()
    }
}

impl ActivePanePipe {
    pub(crate) fn spawn(
        profile: &TerminalProfile,
        pane_output: PaneOutputSender,
        pane_master: PtyMaster,
        command: &str,
        read_from_pipe: bool,
        write_to_pipe: bool,
    ) -> Result<Self, RmuxError> {
        let mut child = tmux_pipe_shell_command(profile, command);
        child.current_dir(profile.cwd());
        child.env_clear();
        child.stdin(if write_to_pipe {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        child.stdout(if read_from_pipe {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        child.stderr(if read_from_pipe {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        for (name, value) in profile.environment() {
            child.env(name, value);
        }
        let mut child = ProcessTreeChild::spawn_with_console_window(
            &mut child,
            ConsoleWindowBehavior::Suppress,
        )
        .map_err(|error| {
            RmuxError::Server(format!("failed to spawn pipe-pane command: {error}"))
        })?;
        let process_group = Arc::new(PipeChildProcessGroup::from_controller(child.controller()));
        let pipe_process_group = Arc::clone(&process_group);
        let stdin = child.child_mut().stdin.take();
        let stdout = child.child_mut().stdout.take();
        let stderr = child.child_mut().stderr.take();
        let (stop_tx, stop_rx) = watch::channel(false);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let pipe_stop_flag = stop_flag.clone();
        let stderr_master = stderr.as_ref().and_then(|_| pane_master.try_clone().ok());
        let pane_output_rx = stdin.as_ref().map(|_| pane_output.subscribe());
        #[cfg(windows)]
        let output_abort = Arc::new(StdMutex::new(None));
        #[cfg(windows)]
        let output_abort_for_task = output_abort.clone();

        tokio::spawn(async move {
            let mut async_tasks = Vec::new();
            let mut blocking_tasks = Vec::new();
            if let Some(stdin) = stdin {
                let (pipe_tx, pipe_rx) = mpsc::channel(64);
                spawn_pipe_thread(
                    &mut blocking_tasks,
                    "rmux-pipe-pane-stdin",
                    stop_flag.clone(),
                    move |stop_flag| forward_pane_bytes_to_pipe(pipe_rx, stdin, stop_flag),
                );
                let output_task = tokio::spawn(forward_pane_output_to_pipe(
                    stop_rx.clone(),
                    stop_flag.clone(),
                    pane_output_rx.expect("stdin pipe must have a pane output subscriber"),
                    pipe_tx,
                ));
                #[cfg(windows)]
                if let Ok(mut output_abort) = output_abort_for_task.lock() {
                    *output_abort = Some(output_task.abort_handle());
                }
                async_tasks.push(output_task);
            }
            if let Some(stdout) = stdout {
                spawn_pipe_thread(
                    &mut blocking_tasks,
                    "rmux-pipe-pane-stdout",
                    stop_flag.clone(),
                    move |stop_flag| forward_pipe_output_to_pane(stdout, pane_master, stop_flag),
                );
            }
            if let Some((stderr, pane_master)) = stderr.zip(stderr_master) {
                spawn_pipe_thread(
                    &mut blocking_tasks,
                    "rmux-pipe-pane-stderr",
                    stop_flag.clone(),
                    move |stop_flag| forward_pipe_output_to_pane(stderr, pane_master, stop_flag),
                );
            }

            let child_stop = stop_flag.clone();
            let child_process_group = Arc::clone(&process_group);
            let mut child_wait = tokio::task::spawn_blocking(move || {
                wait_for_pipe_child(child, child_stop, child_process_group)
            });
            let mut stop_wait = stop_rx.clone();
            let completed_child = tokio::select! {
                _ = wait_for_pipe_stop(&mut stop_wait) => {
                    stop_flag.store(true, Ordering::SeqCst);
                    process_group.terminate();
                    child_wait.await.ok()
                }
                result = &mut child_wait => {
                    // The child exited normally. Keep output forwarders alive
                    // until stdout/stderr reach EOF so short pipe-pane
                    // commands cannot lose their last bytes under load.
                    result.ok()
                }
            };
            for task in async_tasks {
                task.abort();
                let _ = task.await;
            }
            let cleanup_process_group = Arc::clone(&process_group);
            let _ = tokio::task::spawn_blocking(move || {
                for task in blocking_tasks {
                    let _ = task.join();
                }
                // Keep an exited Unix leader waitable until every forwarder
                // has released the tree. Its PGID cannot be recycled before
                // this final one-shot termination and reap.
                cleanup_process_group.terminate();
                if let Some(mut child) = completed_child {
                    let _ = child.wait();
                }
            })
            .await;
        });

        Ok(Self {
            stop_tx,
            stop_flag: pipe_stop_flag,
            process_group: pipe_process_group,
            #[cfg(windows)]
            output_abort,
        })
    }

    pub(crate) fn stop(self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        // This is independent of the child waiter so a descendant that keeps
        // stdout/stderr open after the shell exits cannot strand a forwarder.
        self.process_group.terminate();
        #[cfg(windows)]
        if let Ok(mut output_abort) = self.output_abort.lock() {
            if let Some(output_abort) = output_abort.take() {
                output_abort.abort();
            }
        }
        let _ = self.stop_tx.send(true);
    }

    #[cfg(test)]
    fn process_group_probe(&self) -> PipeProcessGroupProbe {
        PipeProcessGroupProbe(Arc::clone(&self.process_group))
    }
}

async fn wait_for_pipe_stop(stop_rx: &mut watch::Receiver<bool>) {
    while !*stop_rx.borrow() {
        if stop_rx.changed().await.is_err() {
            break;
        }
    }
}

async fn forward_pane_output_to_pipe(
    mut stop_rx: watch::Receiver<bool>,
    stop_flag: Arc<AtomicBool>,
    mut pane_output: PaneOutputReceiver,
    pipe_tx: mpsc::Sender<Vec<u8>>,
) {
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        tokio::select! {
            biased;
            _ = wait_for_pipe_stop(&mut stop_rx) => break,
            next = pane_output.recv() => {
                match next {
                    OutputCursorItem::Event(event) => {
                        if stop_flag.load(Ordering::Relaxed) || *stop_rx.borrow() {
                            break;
                        }
                        let bytes = event.into_bytes();
                        if bytes.is_empty() {
                            break;
                        }
                        if pipe_tx.send(bytes).await.is_err() {
                            break;
                        }
                    }
                    OutputCursorItem::Gap(_) => continue,
                }
            }
        }
    }
}

fn forward_pane_bytes_to_pipe(
    mut pipe_rx: mpsc::Receiver<Vec<u8>>,
    mut stdin: std::process::ChildStdin,
    stop_flag: Arc<AtomicBool>,
) {
    while !stop_flag.load(Ordering::Relaxed) {
        let Some(bytes) = pipe_rx.blocking_recv() else {
            break;
        };
        if bytes.is_empty() || stdin.write_all(&bytes).is_err() {
            break;
        }
    }
    let _ = stdin.flush();
}

fn forward_pipe_output_to_pane<R>(mut reader: R, pane_master: PtyMaster, stop_flag: Arc<AtomicBool>)
where
    R: Read,
{
    let mut buffer = [0_u8; 8192];
    while !stop_flag.load(Ordering::Relaxed) {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                if pane_master.write_all(&buffer[..size]).is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(unix)]
fn tmux_pipe_shell_command(profile: &TerminalProfile, command: &str) -> std::process::Command {
    crate::terminal::shell_std_command(std::path::Path::new("/bin/sh"), profile.cwd(), command)
}

#[cfg(not(unix))]
fn tmux_pipe_shell_command(profile: &TerminalProfile, command: &str) -> std::process::Command {
    profile.shell_std_command(command)
}

fn spawn_pipe_thread<F>(
    tasks: &mut Vec<JoinHandle<()>>,
    name: &'static str,
    stop_flag: Arc<AtomicBool>,
    task: F,
) where
    F: FnOnce(Arc<AtomicBool>) + Send + 'static,
{
    if let Ok(handle) = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || task(stop_flag))
    {
        tasks.push(handle);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn pipe_pane_uses_bin_sh_instead_of_profile_shell_like_tmux() {
        use rmux_core::{EnvironmentStore, OptionStore};
        use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode};

        let root = std::env::temp_dir().join(format!(
            "rmux-pipe-pane-bin-sh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("temp dir exists");
        let fake_shell = root.join("fake-shell.sh");
        std::fs::write(&fake_shell, "#!/bin/sh\nexit 42\n").expect("write fake shell");
        let mut permissions = std::fs::metadata(&fake_shell)
            .expect("fake shell metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_shell, permissions).expect("fake shell permissions");

        let environment = EnvironmentStore::new();
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                fake_shell.to_string_lossy().into_owned(),
                SetOptionMode::Replace,
            )
            .expect("default-shell succeeds");
        let session_name = SessionName::new("pipe-shell").expect("valid session name");
        let profile = TerminalProfile::for_session(
            &environment,
            &options,
            &session_name,
            7,
            root.join("socket").as_path(),
            None,
            false,
            None,
            Some(rmux_core::PaneId::new(3)),
            Some(root.as_path()),
        )
        .expect("profile");

        assert_eq!(profile.shell(), fake_shell.as_path());
        let command = tmux_pipe_shell_command(&profile, "printf ok");
        assert_eq!(command.get_program(), std::path::Path::new("/bin/sh"));

        let _ = std::fs::remove_file(&fake_shell);
        let _ = std::fs::remove_dir_all(&root);
    }
}
