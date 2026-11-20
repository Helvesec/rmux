use std::time::Duration;

use rmux_proto::{RmuxError, SessionName};

use crate::pane_terminals::{
    CompletedDeferredInitialPane, DeferredInitialPaneIdentity, DeferredInitialPaneInputDrain,
    DeferredInitialPaneSpawn, DeferredInputTally, HandlerState, DEFERRED_INPUT_PANE_STOPPED,
};

use super::super::RequestHandler;
#[cfg(test)]
use super::deferred_initial_gate;

#[cfg(windows)]
const DEFERRED_INITIAL_PANE_READY_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(windows)]
const DEFERRED_INITIAL_PANE_READY_SETTLE: Duration = Duration::from_millis(100);
#[cfg(windows)]
// Keep immediate post-new-session console control input queued until the
// autostarted ConPTY console is safely isolated from the launching client.
const DEFERRED_INITIAL_PANE_INPUT_GRACE: Duration = Duration::from_secs(2);

impl RequestHandler {
    #[cfg(windows)]
    pub(super) fn spawn_deferred_initial_pane(&self, job: DeferredInitialPaneSpawn) {
        use tracing::instrument::WithSubscriber;
        let handler = self.clone();
        let task = async move {
            handler.run_deferred_initial_pane_spawn(job).await;
        }
        .with_current_subscriber();
        let _task = if let Some(runtime) = self.server_task_runtime() {
            runtime.spawn(task)
        } else {
            tokio::spawn(task)
        };
        #[cfg(test)]
        self.initial_pane_tasks
            .lock()
            .expect("initial pane tasks")
            .push(_task);
    }

    #[cfg(windows)]
    async fn run_deferred_initial_pane_spawn(&self, job: DeferredInitialPaneSpawn) {
        let open_job = job.clone();
        // Preserve the caller's dispatch through both deferred startup hops.
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let opened = tokio::task::spawn_blocking(move || {
            tracing::dispatcher::with_default(&dispatch, || {
                HandlerState::open_deferred_initial_pane_terminal(&open_job)
            })
        })
        .await;
        let terminal = match opened {
            Ok(Ok(terminal)) => terminal,
            Ok(Err(error)) => {
                self.fail_deferred_initial_pane_spawn(job, error).await;
                return;
            }
            Err(error) => {
                self.fail_deferred_initial_pane_spawn(
                    job,
                    RmuxError::Server(format!("deferred pane spawn task failed: {error}")),
                )
                .await;
                return;
            }
        };

        let completed = {
            let mut state = self.state.lock().await;
            state.complete_deferred_initial_pane_spawn(job.clone(), terminal)
        };
        match completed {
            Ok(Some(completed)) => {
                self.finish_deferred_initial_pane_spawn(completed).await;
            }
            Ok(None) => {}
            Err(error) => {
                self.fail_deferred_initial_pane_spawn(job, error).await;
            }
        }
    }

    #[cfg(windows)]
    async fn finish_deferred_initial_pane_spawn(&self, completed: CompletedDeferredInitialPane) {
        let identity = completed.identity;
        let mut runtime_session_name_hint = completed.runtime_session_name_hint.clone();
        let mut pending = completed.input_writer.map(|input_writer| {
            crate::pane_terminals::DeferredInitialPaneInputFlush {
                input_writer,
                pane_pid: completed.pane_pid,
                queued_input: completed.queued_input,
            }
        });

        if let Some(current_runtime_session_name) = self
            .wait_for_deferred_initial_pane_ready(&runtime_session_name_hint, identity)
            .await
        {
            runtime_session_name_hint = current_runtime_session_name;
        }

        // An existing boundary: the child is running and its output already
        // reaches the transcript, while the pane is still inside the production
        // `Starting` window that the loop below closes. A test may hold it here
        // to observe a real capability announcement. Nothing is stamped, no
        // sink is selected, and no session outside a test is ever gated.
        #[cfg(test)]
        deferred_initial_gate::wait_if_gated(&runtime_session_name_hint).await;

        let input_grace_deadline = tokio::time::Instant::now() + DEFERRED_INITIAL_PANE_INPUT_GRACE;
        loop {
            if let Some(flush) = pending {
                let now = tokio::time::Instant::now();
                if now < input_grace_deadline {
                    pending = Some(flush);
                    tokio::time::sleep(input_grace_deadline - now).await;
                    continue;
                }
                {
                    let mut state = self.state.lock().await;
                    let still_starting = state
                        .starting_runtime_session_for_identity(&runtime_session_name_hint, identity)
                        .is_some();
                    if !still_starting {
                        state.report_discarded_deferred_input(
                            DEFERRED_INPUT_PANE_STOPPED,
                            identity.pane_id(),
                            DeferredInputTally::of(&flush.queued_input),
                        );
                        break;
                    }
                }
                let write_result = Self::flush_deferred_initial_pane_input(flush).await;
                if let Err((error, undelivered)) = write_result {
                    self.state
                        .lock()
                        .await
                        .finish_deferred_initial_pane_input_after_error(
                            &runtime_session_name_hint,
                            identity,
                            &error,
                            undelivered,
                        );
                    break;
                }
            }

            let now = tokio::time::Instant::now();
            if now < input_grace_deadline {
                pending = None;
                tokio::time::sleep(input_grace_deadline - now).await;
                continue;
            }

            let drained = {
                let mut state = self.state.lock().await;
                state.take_deferred_initial_pane_input_or_finish(
                    &runtime_session_name_hint,
                    identity,
                )
            };
            match drained {
                Ok(DeferredInitialPaneInputDrain::Flush {
                    runtime_session_name,
                    flush,
                }) => {
                    runtime_session_name_hint = runtime_session_name;
                    pending = Some(flush);
                }
                Ok(DeferredInitialPaneInputDrain::Finished {
                    runtime_session_name,
                }) => {
                    runtime_session_name_hint = runtime_session_name;
                    break;
                }
                Ok(DeferredInitialPaneInputDrain::Missing) => {
                    break;
                }
                Err(error) => {
                    self.state
                        .lock()
                        .await
                        .finish_deferred_initial_pane_input_after_error(
                            &runtime_session_name_hint,
                            identity,
                            &error,
                            DeferredInputTally::default(),
                        );
                    break;
                }
            }
        }

        self.refresh_deferred_initial_pane(&runtime_session_name_hint, identity)
            .await;
    }

    #[cfg(windows)]
    async fn wait_for_deferred_initial_pane_ready(
        &self,
        runtime_session_name_hint: &SessionName,
        identity: DeferredInitialPaneIdentity,
    ) -> Option<SessionName> {
        let (runtime_session_name, mut receiver) = ({
            let state = self.state.lock().await;
            let runtime_session_name =
                state.starting_runtime_session_for_identity(runtime_session_name_hint, identity)?;
            let receiver = state.subscribe_runtime_pane_output_from_oldest(
                &runtime_session_name,
                identity.pane_id(),
            )?;
            Some((runtime_session_name, receiver))
        })?;

        let deadline = tokio::time::Instant::now() + DEFERRED_INITIAL_PANE_READY_TIMEOUT;
        loop {
            while let Some(item) = receiver.try_recv() {
                if deferred_initial_pane_ready_item(&item) {
                    tokio::time::sleep(DEFERRED_INITIAL_PANE_READY_SETTLE).await;
                    return Some(runtime_session_name);
                }
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Some(runtime_session_name);
            }

            match tokio::time::timeout(deadline - now, receiver.recv()).await {
                Ok(item) if deferred_initial_pane_ready_item(&item) => {
                    tokio::time::sleep(DEFERRED_INITIAL_PANE_READY_SETTLE).await;
                    return Some(runtime_session_name);
                }
                Ok(_) => {}
                Err(_) => return Some(runtime_session_name),
            }
        }
    }

    #[cfg(windows)]
    async fn refresh_deferred_initial_pane(
        &self,
        runtime_session_name_hint: &SessionName,
        identity: DeferredInitialPaneIdentity,
    ) {
        let refresh_identity = {
            let state = self.state.lock().await;
            let runtime_session_name = state.resolve_pane_event_runtime_session(
                runtime_session_name_hint,
                identity.pane_id(),
                Some(identity.generation()),
            );
            runtime_session_name.and_then(|runtime_session_name| {
                let target = state
                    .pane_target_for_runtime_pane(&runtime_session_name, identity.pane_id())?;
                let session = state.sessions.session(target.session_name())?;
                Some((target.session_name().clone(), session.id()))
            })
        };
        if let Some((session_name, session_id)) = refresh_identity {
            self.refresh_attached_session_for_session_identity(&session_name, session_id)
                .await;
        }
    }

    /// Delivers one deferred batch in order. On failure it returns the error
    /// and the part of the batch the pane may not have received.
    #[cfg(windows)]
    async fn flush_deferred_initial_pane_input(
        flush: crate::pane_terminals::DeferredInitialPaneInputFlush,
    ) -> Result<(), (RmuxError, DeferredInputTally)> {
        if flush.queued_input.is_empty() {
            return Ok(());
        }
        let batch = DeferredInputTally::of(&flush.queued_input);
        tokio::task::spawn_blocking(move || {
            let pane_pid = rmux_pty::ProcessId::new(flush.pane_pid)
                .map_err(|error| (std::io::Error::other(error.to_string()), batch))?;
            crate::windows_console_input::deliver_in_order(flush.queued_input, |input| {
                Self::write_deferred_initial_pane_input(&flush.input_writer, pane_pid, input)
            })
            .map_err(|undelivered| {
                (
                    undelivered.error,
                    DeferredInputTally::of(&undelivered.remaining),
                )
            })
        })
        .await
        .map_err(|error| {
            // The task did not report how far it got, so the whole batch is
            // unconfirmed.
            (
                RmuxError::Server(format!("deferred pane input task failed: {error}")),
                batch,
            )
        })?
        .map_err(|(error, undelivered)| {
            (
                RmuxError::Server(format!("failed to flush deferred pane input: {error}")),
                undelivered,
            )
        })
    }

    #[cfg(windows)]
    fn write_deferred_initial_pane_input(
        input_writer: &rmux_pty::PtyMaster,
        pane_pid: rmux_pty::ProcessId,
        input: &crate::pane_terminals::DeferredInitialPaneInput,
    ) -> std::io::Result<()> {
        match input {
            crate::pane_terminals::DeferredInitialPaneInput::Bytes(bytes) => {
                input_writer.write_all(bytes)
            }
            // A pasted body queued during startup takes the sink the live path
            // would have chosen for it, through the same decision: the console
            // records are what keep a legacy ConPTY from parsing the pasted
            // bytes, whether or not an envelope surrounds them.
            crate::pane_terminals::DeferredInitialPaneInput::Paste { bytes, delimiters } => {
                match super::super::pane_support::windows_paste_sink(
                    input_writer.preserves_verbatim_input(),
                    bytes,
                    *delimiters,
                ) {
                    super::super::pane_support::WindowsPasteSink::Pty => {
                        input_writer.write_all(bytes)
                    }
                    super::super::pane_support::WindowsPasteSink::ConsoleUtf8 => {
                        rmux_pty::write_windows_console_utf8(pane_pid, bytes)
                    }
                    super::super::pane_support::WindowsPasteSink::RejectNonUtf8 => {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            super::super::pane_support::LEGACY_CONPTY_NON_UTF8_BRACKETED_PASTE_ERROR,
                        ))
                    }
                }
            }
            crate::pane_terminals::DeferredInitialPaneInput::Console { action, .. } => {
                Self::write_deferred_initial_console_input(pane_pid, *action)
            }
        }
    }

    #[cfg(windows)]
    fn write_deferred_initial_console_input(
        pane_pid: rmux_pty::ProcessId,
        action: crate::pane_terminals::DeferredInitialPaneConsoleInputAction,
    ) -> std::io::Result<()> {
        match action {
            crate::pane_terminals::DeferredInitialPaneConsoleInputAction::Key(key) => {
                crate::windows_console_input::write_with_transient_retry(|| {
                    rmux_pty::write_windows_console_key(pane_pid, key)
                })
            }
            crate::pane_terminals::DeferredInitialPaneConsoleInputAction::KeyThenInterrupt(key) => {
                crate::windows_console_input::write_console_key_then_processed_interrupt(
                    || rmux_pty::write_windows_console_key_reporting_processed_input(pane_pid, key),
                    || rmux_pty::send_windows_console_interrupt(pane_pid),
                )
            }
            crate::pane_terminals::DeferredInitialPaneConsoleInputAction::Interrupt => {
                crate::windows_console_input::write_with_transient_retry(|| {
                    rmux_pty::send_windows_console_interrupt(pane_pid)
                })
            }
        }
    }

    #[cfg(windows)]
    async fn fail_deferred_initial_pane_spawn(
        &self,
        job: DeferredInitialPaneSpawn,
        error: RmuxError,
    ) {
        let (exit_callback, exit_event) = {
            let mut state = self.state.lock().await;
            let exit_event = state.fail_deferred_initial_pane_spawn(&job, &error);
            let exit_callback = exit_event
                .as_ref()
                .and_then(|_| job.pane_exit_callback.clone());
            (exit_callback, exit_event)
        };
        if let (Some(callback), Some(event)) = (exit_callback, exit_event) {
            callback(event);
        }
        self.refresh_attached_session(&job.visible_session_name)
            .await;
        self.refresh_control_session(&job.visible_session_name)
            .await;
    }
}

#[cfg(windows)]
fn deferred_initial_pane_ready_item(item: &rmux_core::events::OutputCursorItem) -> bool {
    matches!(item, rmux_core::events::OutputCursorItem::Event(event) if !event.bytes().is_empty())
}
