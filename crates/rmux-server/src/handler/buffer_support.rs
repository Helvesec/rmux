use std::path::{Path, PathBuf};

use rmux_core::{LifecycleEvent, ScreenCaptureRange};
use rmux_proto::{
    CapturePaneResponse, ClearHistoryResponse, CommandOutput, DeleteBufferResponse, ErrorResponse,
    ListBuffersResponse, LoadBufferResponse, Response, RmuxError, SaveBufferResponse,
    SetBufferResponse, ShowBufferResponse,
};

use super::RequestHandler;
use crate::buffer_file_io;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::AttachControl;
use crate::pane_terminals::PaneCaptureRequest;

mod capture_format;
#[cfg(test)]
#[path = "../handler_buffer/identity_test_pause.rs"]
mod identity_test_pause;
mod list;
mod paste;
mod store;

use capture_format::{apply_capture_format_flags, capture_render_options};
#[cfg(test)]
pub(super) use identity_test_pause::install_paste_buffer_identity_pause;
use list::{
    command_output_from_lines, render_list_buffer_line, sort_buffer_entries, BufferSortOrder,
};
pub(super) use paste::OrderedPasteBufferResult;

impl RequestHandler {
    pub(super) async fn handle_set_buffer(
        &self,
        requester_pid: u32,
        request: rmux_proto::SetBufferRequest,
    ) -> Response {
        let target_client = request.target_client.clone();
        if let Some(new_name) = request.new_name {
            return match self.rename_buffer(request.name, new_name).await {
                Ok(buffer_name) => Response::SetBuffer(SetBufferResponse { buffer_name }),
                Err(error) => Response::Error(ErrorResponse { error }),
            };
        }

        if request.content.is_empty() {
            return Response::SetBuffer(SetBufferResponse {
                buffer_name: String::new(),
            });
        }

        match self
            .store_buffer_with_append(request.name, request.content, request.append)
            .await
        {
            Ok((buffer_name, stored_content)) => {
                if request.set_clipboard {
                    self.copy_bytes_to_attached_clipboard(
                        requester_pid,
                        "set-buffer",
                        &stored_content,
                        target_client.as_deref(),
                    )
                    .await;
                }
                Response::SetBuffer(SetBufferResponse { buffer_name })
            }
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_show_buffer(
        &self,
        request: rmux_proto::ShowBufferRequest,
    ) -> Response {
        let state = self.state.lock().await;

        match state.buffers.show(request.name.as_deref()) {
            Ok((_name, content)) => Response::ShowBuffer(ShowBufferResponse {
                output: CommandOutput::from_stdout(content.to_vec()),
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_list_buffers(
        &self,
        request: rmux_proto::ListBuffersRequest,
    ) -> Response {
        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        let sort_order = match BufferSortOrder::parse(request.sort_order.as_deref()) {
            Some(order) => order,
            None => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(rmux_core::INVALID_SORT_ORDER.to_owned()),
                });
            }
        };

        let mut entries = state.buffers.entries();
        sort_buffer_entries(
            &mut entries,
            sort_order,
            request.reversed && request.sort_order.is_some(),
        );
        let lines = entries
            .into_iter()
            .filter_map(|entry| render_list_buffer_line(&state, &socket_path, &request, entry))
            .collect::<Vec<_>>();

        Response::ListBuffers(ListBuffersResponse {
            output: command_output_from_lines(&lines),
        })
    }

    pub(super) async fn handle_delete_buffer(
        &self,
        request: rmux_proto::DeleteBufferRequest,
    ) -> Response {
        let capacity = match self.pane_mode_post_commit.acquire_capacity().await {
            Ok(capacity) => capacity,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let post_commit = capacity.sequence();
        let handler = self.clone();
        match post_commit
            .run_durable(async move {
                let transaction = handler.pane_mode_transaction.clone().lock_owned().await;
                let (buffer_name, event) = {
                    let mut state = handler.state.lock().await;
                    let buffer_name = state.buffers.delete(request.name.as_deref())?;
                    let event = super::prepare_lifecycle_event(
                        &mut state,
                        &LifecycleEvent::PasteBufferDeleted {
                            buffer_name: buffer_name.clone(),
                        },
                    );
                    (buffer_name, event)
                };
                drop(transaction);
                handler.emit_prepared(event).await;
                Ok::<_, RmuxError>(buffer_name)
            })
            .await
        {
            Ok(Ok(buffer_name)) => Response::DeleteBuffer(DeleteBufferResponse { buffer_name }),
            Ok(Err(error)) | Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_load_buffer(
        &self,
        requester_pid: u32,
        request: rmux_proto::LoadBufferRequest,
    ) -> Response {
        let target_client = request.target_client.clone();
        let resolved_path = resolve_buffer_path(&request.path, request.cwd.as_deref());
        let content = match buffer_file_io::read(resolved_path).await {
            Ok(content) => content,
            Err(error) => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!(
                        "failed to read buffer file '{}': {error}",
                        request.path
                    )),
                });
            }
        };
        if content.is_empty() {
            return Response::LoadBuffer(LoadBufferResponse {
                buffer_name: String::new(),
            });
        }

        let clipboard_bytes = request.set_clipboard.then_some(content.clone());
        match self.store_buffer(request.name, content).await {
            Ok(buffer_name) => {
                if let Some(bytes) = clipboard_bytes.as_deref() {
                    self.copy_bytes_to_attached_clipboard(
                        requester_pid,
                        "load-buffer",
                        bytes,
                        target_client.as_deref(),
                    )
                    .await;
                }
                Response::LoadBuffer(LoadBufferResponse { buffer_name })
            }
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_save_buffer(
        &self,
        request: rmux_proto::SaveBufferRequest,
    ) -> Response {
        let (buffer_name, content) = {
            let state = self.state.lock().await;
            match state.buffers.show(request.name.as_deref()) {
                Ok((name, content)) => (name.to_owned(), content.to_vec()),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        let resolved_path = resolve_buffer_path(&request.path, request.cwd.as_deref());
        let save_result = buffer_file_io::write(resolved_path, content, request.append).await;
        match save_result {
            Ok(()) => Response::SaveBuffer(SaveBufferResponse { buffer_name }),
            Err(error) => Response::Error(ErrorResponse {
                error: RmuxError::Server(format!(
                    "failed to write buffer file '{}': {error}",
                    request.path
                )),
            }),
        }
    }

    pub(super) async fn handle_capture_pane(
        &self,
        request: rmux_proto::CapturePaneRequest,
    ) -> Response {
        let (mut content, line_format) = {
            let mut state = self.state.lock().await;
            if let Err(error) = super::require_expected_pane_identity(&state, &request.target) {
                return Response::Error(ErrorResponse { error });
            }
            let range = ScreenCaptureRange {
                start: request.start,
                end: request.end,
                start_is_absolute: request.start_is_absolute,
                end_is_absolute: request.end_is_absolute,
            };
            let options = capture_render_options(&request);
            let capture_request = PaneCaptureRequest {
                range,
                options,
                alternate: request.alternate,
                use_mode_screen: request.use_mode_screen,
                pending_input: request.pending_input,
                quiet: request.quiet,
                escape_pending: request.escape_sequences,
                hyperlinks: request.hyperlinks,
            };
            match state.capture_transcript_for_command(
                &request.target,
                capture_request,
                request.include_format || request.line_numbers,
            ) {
                Ok(captured) => captured,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };
        apply_capture_format_flags(&mut content, &request, line_format.as_ref());

        if request.print {
            let mut stdout = content;
            if !stdout.ends_with(b"\n") {
                stdout.push(b'\n');
            }
            return Response::CapturePane(CapturePaneResponse::from_output(
                CommandOutput::from_stdout(stdout),
            ));
        }

        match self.store_buffer(request.buffer_name, content).await {
            Ok(buffer_name) => Response::CapturePane(CapturePaneResponse::from_buffer(buffer_name)),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_clear_history(
        &self,
        request: rmux_proto::ClearHistoryRequest,
    ) -> Response {
        let mut state = self.state.lock().await;
        match state.clear_history(&request.target, request.reset_hyperlinks) {
            Ok(()) => Response::ClearHistory(ClearHistoryResponse),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    async fn rename_buffer(
        &self,
        old_name: Option<String>,
        new_name: String,
    ) -> Result<String, RmuxError> {
        let capacity = self.pane_mode_post_commit.acquire_capacity().await?;
        let post_commit = capacity.sequence();
        let handler = self.clone();
        post_commit
            .run_durable(async move {
                let transaction = handler.pane_mode_transaction.clone().lock_owned().await;
                let (buffer_name, events) = {
                    let mut state = handler.state.lock().await;
                    let outcome = state.buffers.rename(old_name.as_deref(), &new_name)?;
                    let mut lifecycle_events = Vec::new();
                    if outcome.changed() {
                        if outcome.replaced() {
                            lifecycle_events.push(LifecycleEvent::PasteBufferDeleted {
                                buffer_name: outcome.new_name().to_owned(),
                            });
                        }
                        lifecycle_events.push(LifecycleEvent::PasteBufferDeleted {
                            buffer_name: outcome.old_name().to_owned(),
                        });
                        lifecycle_events.push(LifecycleEvent::PasteBufferChanged {
                            buffer_name: outcome.new_name().to_owned(),
                        });
                    }
                    let events = lifecycle_events
                        .iter()
                        .map(|event| super::prepare_lifecycle_event(&mut state, event))
                        .collect::<Vec<_>>();
                    (outcome.new_name().to_owned(), events)
                };
                drop(transaction);
                for event in events {
                    handler.emit_prepared(event).await;
                }
                Ok::<_, RmuxError>(buffer_name)
            })
            .await?
    }

    async fn copy_bytes_to_attached_clipboard(
        &self,
        requester_pid: u32,
        command_name: &str,
        bytes: &[u8],
        target_client: Option<&str>,
    ) {
        let target = match target_client {
            Some(target_client) => {
                let Ok(Some(attach_pid)) = self
                    .find_target_attach_client_pid(requester_pid, target_client, command_name)
                    .await
                else {
                    return;
                };
                self.terminal_context_for_attached_client(attach_pid)
                    .await
                    .map(|terminal_context| (attach_pid, terminal_context))
            }
            None => {
                self.clipboard_attach_for_requester(requester_pid, command_name)
                    .await
            }
        };
        let Some((attach_pid, terminal_context)) = target else {
            return;
        };
        let payload = {
            let state = self.state.lock().await;
            OuterTerminal::resolve(&state.options, terminal_context)
                .encode_forced_clipboard_set(bytes)
        };
        let Some(payload) = payload else {
            return;
        };
        let _ = self
            .send_attach_control(attach_pid, AttachControl::Write(payload), command_name)
            .await;
    }
}

fn resolve_buffer_path(path: &str, cwd: Option<&Path>) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if let Some(cwd) = cwd {
        cwd.join(candidate)
    } else {
        candidate.to_path_buf()
    }
}
