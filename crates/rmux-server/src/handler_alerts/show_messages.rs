//! `show-messages` command handling and terminal/job listing.

use rmux_core::formats::FormatContext;
use rmux_proto::{ErrorResponse, Response, RmuxError, ShowMessagesResponse, Target};

use super::super::{
    command_output_from_lines, control_support::ManagedClient, overlay_support::ClientOverlayState,
    scripting_support::format_context_for_target, RequestHandler,
};
use super::{JobSummary, TerminalSummary, SHOW_MESSAGES_TEMPLATE};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

impl RequestHandler {
    pub(in crate::handler) async fn handle_show_messages(
        &self,
        requester_pid: u32,
        request: rmux_proto::ShowMessagesRequest,
    ) -> Response {
        if request.terminals || request.jobs {
            let filter = match self
                .resolve_show_messages_target_client(
                    requester_pid,
                    request.target_client.as_deref(),
                )
                .await
            {
                Ok(filter) => filter,
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

        let attached_session = self.current_session_candidate(requester_pid).await;
        if attached_session.is_none() {
            return Response::Error(ErrorResponse {
                error: RmuxError::Message("no current client".to_owned()),
            });
        }
        let lines = {
            let state = self.state.lock().await;
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
    ) -> Result<Option<u32>, RmuxError> {
        let Some(target_client) = target_client else {
            return Ok(None);
        };
        if target_client == "=" {
            return self
                .resolve_managed_client(requester_pid, "show-messages")
                .await
                .map(|client| match client {
                    ManagedClient::Attach(pid) | ManagedClient::Control(pid) => Some(pid),
                });
        }

        target_client
            .parse::<u32>()
            .map(Some)
            .map_err(|_| RmuxError::invalid_target(target_client, "invalid client identifier"))
    }

    async fn show_message_terminals(&self, filter: Option<u32>) -> Vec<String> {
        let terminals = {
            let active_attach = self.active_attach.lock().await;
            let mut terminals = active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (filter.is_none() || filter == Some(*pid)).then_some(TerminalSummary {
                        attach_pid: *pid,
                        session_name: active.session_name.clone(),
                        cols: active.client_size.cols,
                        rows: active.client_size.rows,
                    })
                })
                .collect::<Vec<_>>();
            terminals.sort_by_key(|terminal| terminal.attach_pid);
            terminals
        };

        terminals
            .into_iter()
            .enumerate()
            .map(|(index, terminal)| {
                format!(
                    "Terminal {index}: attached client {} for {}, size={}x{}",
                    terminal.attach_pid, terminal.session_name, terminal.cols, terminal.rows
                )
            })
            .collect()
    }

    async fn show_message_jobs(&self, filter: Option<u32>) -> Vec<String> {
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
                    (popup_has_job && (filter.is_none() || filter == Some(*pid))).then_some(
                        JobSummary {
                            attach_pid: *pid,
                            session_name: active.session_name.clone(),
                        },
                    )
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
