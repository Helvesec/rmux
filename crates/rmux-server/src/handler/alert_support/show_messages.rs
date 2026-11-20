//! `show-messages` command handling and terminal/job listing.

use rmux_core::{command_parser::CommandArgument, formats::FormatContext};
use rmux_proto::{ErrorResponse, Response, RmuxError, ShowMessagesResponse, Target};

use super::super::{
    command_output_from_lines,
    control_support::ManagedClient,
    overlay_support::ClientOverlayState,
    scripting_support::{format_context_for_target, queued_command_context},
    RequestHandler,
};
use super::{JobSummary, TerminalSummary, SHOW_MESSAGES_TEMPLATE};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::outer_terminal::OuterTerminal;

impl RequestHandler {
    pub(in crate::handler) async fn handle_show_messages(
        &self,
        requester_pid: u32,
        request: rmux_proto::ShowMessagesRequest,
    ) -> Response {
        // Retain the client identity before resolving targets. In the log
        // path, insert the observer and snapshot its messages under one lock
        // so a concurrent command cannot replace the bound observer entry.
        let observer_message = if self.authenticated_requester_peer(requester_pid).is_some()
            || queued_command_context().is_some()
        {
            let client_name = self
                .active_attach
                .lock()
                .await
                .by_pid
                .get(&requester_pid)
                .filter(|active| !active.closing.load(std::sync::atomic::Ordering::SeqCst))
                .map(|active| active.client_name.clone())
                .unwrap_or_else(|| crate::client_names::control_client_name(requester_pid));
            let mut command = String::from("show-messages");
            let mut flags = String::new();
            if request.jobs {
                flags.push('J');
            }
            if request.terminals {
                flags.push('T');
            }
            if !flags.is_empty() {
                command.push_str(&format!(" -{flags}"));
            }
            if let Some(target) = &request.target_client {
                command.push_str(" -t ");
                command.push_str(&CommandArgument::String(target.clone()).to_tmux_string());
            }
            Some(format!("{client_name} command: {command}"))
        } else {
            None
        };
        if request.terminals || request.jobs {
            if let Some(message) = observer_message {
                self.state.lock().await.add_message(message);
            }
            let filter = match self
                .resolve_show_messages_target_client(
                    requester_pid,
                    request.target_client.as_deref(),
                )
                .await
            {
                Ok(filter) => filter,
                Err(error)
                    if request.target_client.is_none()
                        && matches!(
                            &error,
                            RmuxError::Server(message) | RmuxError::Message(message)
                                if message == "no current client"
                        ) =>
                {
                    return Response::ShowMessages(ShowMessagesResponse::from_output(
                        command_output_from_lines(&[]),
                    ));
                }
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let terminals = if request.terminals {
                self.show_message_terminals(filter).await
            } else {
                Vec::new()
            };
            let jobs = if request.jobs {
                self.show_message_jobs(filter).await
            } else {
                Vec::new()
            };
            let mut lines = Vec::new();
            lines.extend(terminals);
            if !lines.is_empty() && !jobs.is_empty() {
                lines.push(String::new());
            }
            lines.extend(jobs);
            return Response::ShowMessages(ShowMessagesResponse::from_output(
                command_output_from_lines(&lines),
            ));
        }

        let attached_session = match request.target_client.as_deref() {
            Some(target_client) => match self
                .resolve_show_messages_log_session(requester_pid, Some(target_client))
                .await
            {
                Ok(session) => session,
                Err(error) => {
                    if let Some(message) = observer_message {
                        self.state.lock().await.add_message(message);
                    }
                    return Response::Error(ErrorResponse { error });
                }
            },
            None => self.current_session_candidate(requester_pid).await,
        };
        let lines = {
            let mut state = self.state.lock().await;
            if let Some(message) = observer_message {
                state.add_message(message);
            }
            state
                .message_log
                .iter()
                .rev()
                .map(|entry| {
                    let context = attached_session
                        .as_ref()
                        .and_then(|session_name| {
                            format_context_for_target(
                                &state,
                                &Target::Session(session_name.clone()),
                                0,
                            )
                            .ok()
                        })
                        .unwrap_or_else(|| {
                            RuntimeFormatContext::new(FormatContext::new()).with_state(&state)
                        })
                        .with_named_value("message_number", entry.msg_num.to_string())
                        .with_named_value("message_text", entry.msg.clone())
                        .with_named_value("message_time", entry.msg_time.to_string());
                    render_runtime_template(SHOW_MESSAGES_TEMPLATE, &context, false)
                })
                .collect::<Vec<_>>()
        };

        Response::ShowMessages(ShowMessagesResponse::from_output(
            command_output_from_lines(&lines),
        ))
    }

    async fn resolve_show_messages_target_client(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
    ) -> Result<ManagedClient, RmuxError> {
        self.resolve_target_managed_client(requester_pid, target_client, "show-messages")
            .await
    }

    async fn resolve_show_messages_log_session(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
    ) -> Result<Option<rmux_proto::SessionName>, RmuxError> {
        match self
            .resolve_target_managed_client(requester_pid, target_client, "show-messages")
            .await?
        {
            ManagedClient::Attach {
                pid: attach_pid,
                attach_id,
            } => {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .filter(|active| {
                        active.id == attach_id
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .map(|active| Some(active.session_name.clone()))
                    .ok_or_else(|| {
                        crate::handler_support::attached_client_required("show-messages")
                    })
            }
            ManagedClient::Control(identity) => {
                let active_control = self.active_control.lock().await;
                active_control
                    .by_pid
                    .get(&identity.requester_pid())
                    .filter(|active| {
                        active.id == identity.control_id()
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .map(|active| active.session_name.clone())
                    .ok_or_else(|| {
                        crate::handler_support::attached_client_required("show-messages")
                    })
            }
        }
    }

    async fn show_message_terminals(&self, filter: ManagedClient) -> Vec<String> {
        let terminals = {
            let active_attach = self.active_attach.lock().await;
            let mut terminals = active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    matches!(
                        filter,
                        ManagedClient::Attach {
                            pid: expected_pid,
                            attach_id: expected_attach_id,
                        } if *pid == expected_pid
                            && active.id == expected_attach_id
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    )
                    .then_some(TerminalSummary {
                        attach_pid: *pid,
                        session_name: active.session_name.clone(),
                        cols: active.client_size.cols,
                        rows: active.client_size.rows,
                        context: active.terminal_context.clone(),
                    })
                })
                .collect::<Vec<_>>();
            terminals.sort_by_key(|terminal| terminal.attach_pid);
            terminals
        };

        let state = self.state.lock().await;
        terminals
            .into_iter()
            .enumerate()
            .flat_map(|(index, terminal)| {
                let mut lines = vec![format!(
                    "Terminal {index}: attached client {} for {}, size={}x{}",
                    terminal.attach_pid, terminal.session_name, terminal.cols, terminal.rows
                )];
                // Resolve the captured client's context through the same options
                // and overrides used by clipboard writes and terminal rendering.
                lines.extend(
                    OuterTerminal::resolve(&state.options, terminal.context).capability_report(),
                );
                lines
            })
            .collect()
    }

    async fn show_message_jobs(&self, filter: ManagedClient) -> Vec<String> {
        let jobs = {
            let active_attach = self.active_attach.lock().await;
            let mut jobs = active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    let popup_has_job = active
                        .overlay
                        .as_ref()
                        .and_then(|overlay| match overlay {
                            ClientOverlayState::Popup(popup) => popup.job.as_ref().map(|_| ()),
                            ClientOverlayState::Menu(_) => None,
                        })
                        .is_some();
                    let exact_attach = matches!(
                        filter,
                        ManagedClient::Attach {
                            pid: expected_pid,
                            attach_id: expected_attach_id,
                        } if *pid == expected_pid
                            && active.id == expected_attach_id
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    );
                    (popup_has_job && exact_attach).then_some(JobSummary {
                        attach_pid: *pid,
                        session_name: active.session_name.clone(),
                    })
                })
                .collect::<Vec<_>>();
            jobs.sort_by_key(|job| job.attach_pid);
            jobs
        };

        jobs.into_iter()
            .enumerate()
            .map(|(index, job)| {
                format!(
                    "Job {index}: popup job for client {} in {}",
                    job.attach_pid, job.session_name
                )
            })
            .collect()
    }
}
