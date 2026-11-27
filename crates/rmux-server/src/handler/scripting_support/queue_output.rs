#[cfg(windows)]
use super::format_references_pane_pid;
use super::{
    queue_action_from_response, CommandOutput, DisplayMessageRequest, PaneId, PaneTarget,
    QueueCommandAction, RequestHandler, Response, RmuxError, Target,
};

impl RequestHandler {
    pub(super) async fn deliver_targeted_run_shell_output(
        &self,
        requester_pid: u32,
        target: &PaneTarget,
        pane_id: PaneId,
        response: &Response,
    ) -> bool {
        let Response::RunShell(response) = response else {
            return false;
        };
        let Some(output) = response.command_output() else {
            return self
                .current_target_for_stable_pane(pane_id, Some(target.session_name()))
                .await
                .is_some();
        };
        let message = String::from_utf8_lossy(output.stdout())
            .trim_end_matches(['\r', '\n'])
            .replace('#', "##");
        if message.is_empty() {
            return true;
        }
        matches!(
            self.handle_display_message_for_stable_pane(
                requester_pid,
                pane_id,
                DisplayMessageRequest {
                    target: Some(Target::Pane(target.clone())),
                    print: false,
                    message: Some(message),
                    empty_target_context: false,
                },
            )
            .await,
            Response::DisplayMessage(_)
        )
    }

    pub(super) async fn pane_id_for_slot_target(&self, target: &PaneTarget) -> Option<PaneId> {
        let state = self.state.lock().await;
        state
            .sessions
            .session(target.session_name())
            .and_then(|session| {
                session.pane_id_in_window(target.window_index(), target.pane_index())
            })
    }

    pub(super) async fn execute_queued_list_panes_all(
        &self,
        command: super::list_parse::ParsedListPanesAllCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        let format = command
            .format
            .unwrap_or_else(|| rmux_core::formats::DEFAULT_LIST_PANES_ALL_FORMAT.to_owned());
        let mut session_names = {
            let state = self.state.lock().await;
            state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        session_names.sort_by_key(ToString::to_string);

        let mut stdout = Vec::new();
        for session_name in session_names {
            let response = self
                .handle_list_panes(rmux_proto::ListPanesRequest {
                    target: session_name,
                    target_window_index: None,
                    format: Some(format.clone()),
                    filter: command.filter.clone(),
                    sort_order: command.sort_order.clone(),
                    reversed: command.reversed,
                })
                .await;
            let action = queue_action_from_response(response)?;
            if let QueueCommandAction::Normal {
                output: Some(output),
                error,
                source_file_error: _,
                exit_status: _,
            } = action
            {
                stdout.extend_from_slice(output.stdout());
                if let Some(error) = error {
                    return Err(error);
                }
            }
        }

        Ok(QueueCommandAction::Normal {
            output: Some(CommandOutput::from_stdout(stdout)),
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }

    pub(super) async fn execute_queued_list_windows_all(
        &self,
        command: super::list_parse::ParsedListWindowsAllCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        #[cfg(windows)]
        if format_references_pane_pid(command.format.as_deref())
            || format_references_pane_pid(command.filter.as_deref())
        {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let session_names = {
            let state = self.state.lock().await;
            state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        };
        let mut attached_counts = std::collections::HashMap::new();
        for session_name in session_names {
            attached_counts.insert(
                session_name.clone(),
                self.attached_count(&session_name).await,
            );
        }
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let response = {
            let state = self.state.lock().await;
            state.list_windows_all(crate::pane_terminals::ListWindowsAllSelection {
                socket_path: &self.socket_path(),
                format: command.format.as_deref(),
                attached_counts: &attached_counts,
                filter: command.filter.as_deref(),
                sort_order: command.sort_order.as_deref(),
                reversed: command.reversed,
                client_loop_snapshots: &client_loop_snapshots,
            })?
        };
        queue_action_from_response(Response::ListWindows(response))
    }
}
