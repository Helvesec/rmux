use std::collections::HashMap;
use std::io;
use std::process::Stdio;

use rmux_core::events::OutputCursorItem;
use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName};
use rmux_pty::PtyMaster;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

use crate::pane_io::{PaneOutputReceiver, PaneOutputSender};
use crate::terminal::TerminalProfile;

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
        let mut child = profile.shell_command(command);
        child.current_dir(profile.cwd());
        child.env_clear();
        child.kill_on_drop(true);
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

        let mut child = child.spawn().map_err(|error| {
            RmuxError::Server(format!("failed to spawn pipe-pane command: {error}"))
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (stop_tx, stop_rx) = watch::channel(false);
        let stderr_master = stderr.as_ref().and_then(|_| pane_master.try_clone().ok());

        tokio::spawn(async move {
            let pane_output_task = stdin.map(|stdin| {
                tokio::spawn(forward_pane_output_to_pipe(
                    stop_rx.clone(),
                    pane_output.subscribe(),
                    stdin,
                ))
            });
            let pipe_stdout_task = stdout.map(|stdout| {
                tokio::spawn(forward_pipe_output_to_pane(
                    stop_rx.clone(),
                    stdout,
                    pane_master,
                ))
            });
            let pipe_stderr_task = stderr.zip(stderr_master).map(|(stderr, pane_master)| {
                tokio::spawn(forward_pipe_output_to_pane(
                    stop_rx.clone(),
                    stderr,
                    pane_master,
                ))
            });
            let mut stop_wait = stop_rx.clone();
            tokio::select! {
                _ = wait_for_pipe_stop(&mut stop_wait) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
                _ = child.wait() => {}
            }

            for task in [pane_output_task, pipe_stdout_task, pipe_stderr_task]
                .into_iter()
                .flatten()
            {
                task.abort();
                let _ = task.await;
            }
        });

        Ok(Self { stop_tx })
    }

    pub(crate) fn stop(self) {
        let _ = self.stop_tx.send(true);
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
    mut pane_output: PaneOutputReceiver,
    mut stdin: tokio::process::ChildStdin,
) {
    loop {
        tokio::select! {
            biased;
            _ = wait_for_pipe_stop(&mut stop_rx) => break,
            next = pane_output.recv() => {
                match next {
                    OutputCursorItem::Event(event) => {
                        if *stop_rx.borrow() {
                            break;
                        }
                        let bytes = event.into_bytes();
                        if bytes.is_empty() {
                            break;
                        }
                        if stdin.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    OutputCursorItem::Gap(_) => continue,
                }
            }
        }
    }
    let _ = stdin.shutdown().await;
}

async fn forward_pipe_output_to_pane<R>(
    mut stop_rx: watch::Receiver<bool>,
    mut reader: R,
    pane_master: PtyMaster,
) where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 8192];
    loop {
        tokio::select! {
            biased;
            _ = wait_for_pipe_stop(&mut stop_rx) => break,
            read = reader.read(&mut buffer) => {
                match read {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        if *stop_rx.borrow() {
                            break;
                        }
                        if write_pipe_output_to_pane(&pane_master, buffer[..size].to_vec())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
async fn write_pipe_output_to_pane(pane_master: &PtyMaster, bytes: Vec<u8>) -> io::Result<()> {
    let pane_master = pane_master.try_clone().map_err(io::Error::other)?;
    tokio::task::spawn_blocking(move || pane_master.write_all(&bytes))
        .await
        .map_err(|error| io::Error::other(format!("pipe-pane write task failed: {error}")))?
}

#[cfg(not(windows))]
async fn write_pipe_output_to_pane(pane_master: &PtyMaster, bytes: Vec<u8>) -> io::Result<()> {
    pane_master.write_all(&bytes)
}
