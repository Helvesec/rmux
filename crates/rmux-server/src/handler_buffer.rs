use std::path::{Path, PathBuf};

use rmux_core::{encode_paste_bytes, LifecycleEvent, ScreenCaptureRange};
use rmux_proto::{
    CapturePaneResponse, ClearHistoryResponse, CommandOutput, DeleteBufferResponse, ErrorResponse,
    ListBuffersResponse, LoadBufferResponse, PasteBufferResponse, Response, RmuxError,
    SaveBufferResponse, SetBufferResponse, ShowBufferResponse,
};

use super::mode_tree_support::ModeTreeActionIdentity;
use super::pane_support::{prepare_pane_input_write, write_bytes_to_target_io, PaneInputLiveness};
use super::RequestHandler;
use crate::buffer_file_io;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, PaneCaptureRequest};

#[path = "handler_buffer/capture_format.rs"]
mod capture_format;
#[cfg(test)]
#[path = "handler_buffer/identity_test_pause.rs"]
mod identity_test_pause;
#[path = "handler_buffer/list.rs"]
mod list;
#[path = "handler_buffer/store.rs"]
mod store;

use capture_format::{apply_capture_format_flags, capture_render_options};
#[cfg(test)]
pub(super) use identity_test_pause::{
    install_paste_buffer_identity_pause, pause_after_paste_buffer_identity_capture,
};
use list::{
    command_output_from_lines, render_list_buffer_line, sort_buffer_entries, BufferSortOrder,
};

#[derive(Debug)]
pub(super) enum OrderedPasteBufferResult {
    /// The selected name now identifies a different buffer instance.
    StaleIdentity,
    /// The mode-tree action no longer belongs to the attached client and
    /// tree generation that selected it.
    StaleRequesterIdentity,
    /// A normal public response, including the legitimate empty-store no-op.
    Completed(Response),
}

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

    pub(super) async fn handle_paste_buffer(
        &self,
        request: rmux_proto::PasteBufferRequest,
    ) -> Response {
        match self.handle_paste_buffer_inner(request, None, None).await {
            OrderedPasteBufferResult::Completed(response) => response,
            OrderedPasteBufferResult::StaleIdentity => Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "unordered paste unexpectedly produced a stale buffer identity".to_owned(),
                ),
            }),
            OrderedPasteBufferResult::StaleRequesterIdentity => Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "unordered paste unexpectedly carried a mode-tree requester identity"
                        .to_owned(),
                ),
            }),
        }
    }

    #[cfg(test)]
    pub(super) async fn handle_paste_buffer_for_order(
        &self,
        request: rmux_proto::PasteBufferRequest,
        expected_order: u64,
    ) -> OrderedPasteBufferResult {
        self.handle_paste_buffer_inner(request, Some(expected_order), None)
            .await
    }

    pub(super) async fn handle_paste_buffer_for_order_and_requester(
        &self,
        request: rmux_proto::PasteBufferRequest,
        expected_order: u64,
        expected_requester: ModeTreeActionIdentity,
    ) -> OrderedPasteBufferResult {
        self.handle_paste_buffer_inner(request, Some(expected_order), Some(expected_requester))
            .await
    }

    async fn handle_paste_buffer_inner(
        &self,
        request: rmux_proto::PasteBufferRequest,
        expected_order: Option<u64>,
        expected_requester: Option<ModeTreeActionIdentity>,
    ) -> OrderedPasteBufferResult {
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let pane_index = request.target.pane_index();

        // This lock is the ordered paste linearization point: validate the
        // monotonic order and snapshot the matching bytes atomically. A later
        // replacement cannot retarget the write, and delete_if_order_matches
        // below prevents deleting that replacement after the async write.
        let (buffer_name, content, buffer_order, pane_id, bracketed_mode) = {
            let state = self.state.lock().await;

            if !state.sessions.contains_session(&session_name) {
                return OrderedPasteBufferResult::Completed(Response::Error(ErrorResponse {
                    error: session_not_found(&session_name),
                }));
            }

            if request.name.is_none() && state.buffers.is_empty() {
                return OrderedPasteBufferResult::Completed(Response::PasteBuffer(
                    PasteBufferResponse {
                        buffer_name: String::new(),
                    },
                ));
            }

            let (name, content, order) =
                match state.buffers.show_with_order(request.name.as_deref()) {
                    Ok((name, content, order)) => (name.to_owned(), content.to_vec(), order),
                    Err(error) => {
                        return OrderedPasteBufferResult::Completed(Response::Error(
                            ErrorResponse { error },
                        ))
                    }
                };
            if expected_order.is_some_and(|expected_order| expected_order != order) {
                return OrderedPasteBufferResult::StaleIdentity;
            }

            let pane = match state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(window_index))
                .and_then(|window| window.pane(pane_index))
            {
                Some(pane) => pane,
                None => {
                    return OrderedPasteBufferResult::Completed(Response::Error(ErrorResponse {
                        error: RmuxError::invalid_target(
                            format!("{session_name}:{window_index}.{pane_index}"),
                            "pane index does not exist in session",
                        ),
                    }))
                }
            };
            let bracketed_mode = request.bracketed
                && state
                    .pane_screen_state(&session_name, pane.id())
                    .is_some_and(|state| {
                        state.mode & rmux_core::input::mode::MODE_BRACKETPASTE != 0
                    });

            (name, content, order, pane.id(), bracketed_mode)
        };

        #[cfg(test)]
        pause_after_paste_buffer_identity_capture(&session_name).await;

        let delete_post_commit = if request.delete_after {
            let capacity = match self.pane_mode_post_commit.acquire_capacity().await {
                Ok(capacity) => capacity,
                Err(error) => {
                    return OrderedPasteBufferResult::Completed(Response::Error(ErrorResponse {
                        error,
                    }))
                }
            };
            Some(capacity.sequence())
        } else {
            None
        };

        let payload = render_paste_payload(&content, &request);
        let payload = bracketed_paste_payload(payload, bracketed_mode);

        if let Some(post_commit) = delete_post_commit {
            let handler = self.clone();
            let target = request.target.clone();
            return match post_commit
                .run_durable(async move {
                    if let Err(result) = handler
                        .write_ordered_paste_payload(&target, pane_id, expected_requester, payload)
                        .await
                    {
                        return result;
                    }
                    handler.pause_before_paste_buffer_delete().await;
                    let transaction = handler.pane_mode_transaction.clone().lock_owned().await;
                    let event = {
                        let mut state = handler.state.lock().await;
                        // The requester and stable pane were validated at the write
                        // linearization point. Once the bytes are observable, caller
                        // cancellation or attach replacement cannot roll that write
                        // back and therefore must not cancel its requested deletion.
                        state
                            .buffers
                            .delete_if_order_matches(&buffer_name, buffer_order)
                            .then(|| {
                                super::prepare_lifecycle_event(
                                    &mut state,
                                    &LifecycleEvent::PasteBufferDeleted {
                                        buffer_name: buffer_name.clone(),
                                    },
                                )
                            })
                    };
                    drop(transaction);
                    if let Some(event) = event {
                        handler.emit_prepared(event).await;
                    }
                    OrderedPasteBufferResult::Completed(Response::PasteBuffer(
                        PasteBufferResponse { buffer_name },
                    ))
                })
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    OrderedPasteBufferResult::Completed(Response::Error(ErrorResponse { error }))
                }
            };
        }

        if let Err(result) = self
            .write_ordered_paste_payload(&request.target, pane_id, expected_requester, payload)
            .await
        {
            return result;
        }

        OrderedPasteBufferResult::Completed(Response::PasteBuffer(PasteBufferResponse {
            buffer_name,
        }))
    }

    async fn write_ordered_paste_payload(
        &self,
        target: &rmux_proto::PaneTarget,
        pane_id: rmux_proto::PaneId,
        expected_requester: Option<ModeTreeActionIdentity>,
        payload: Vec<u8>,
    ) -> Result<(), OrderedPasteBufferResult> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let pane_index = target.pane_index();
        let write = {
            let mut state = self.state.lock().await;
            // The state lock is acquired before the attach lock, matching
            // attach registration. Keeping both through sink preparation
            // makes this requester check the logical commit point for this
            // individual mode-tree paste.
            let _active_attach = match expected_requester {
                Some(expected) => {
                    let active_attach = self.active_attach.lock().await;
                    let requester_is_current = active_attach
                        .by_pid
                        .get(&expected.attach_pid())
                        .is_some_and(|active| {
                            active.id == expected.attach_id()
                                && active.mode_tree_state_id == expected.state_id()
                                && active.mode_tree.is_some()
                                && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                        });
                    if !requester_is_current {
                        return Err(OrderedPasteBufferResult::StaleRequesterIdentity);
                    }
                    Some(active_attach)
                }
                None => None,
            };
            // The wrapper decision belongs to the captured stable pane. Keep
            // identity validation and sink preparation under this same lock so
            // a concurrent slot replacement cannot inherit that decision.
            let pane_identity_matches = state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(window_index))
                .and_then(|window| window.pane(pane_index))
                .is_some_and(|pane| pane.id() == pane_id);
            if !pane_identity_matches {
                return Err(OrderedPasteBufferResult::Completed(Response::Error(
                    ErrorResponse {
                        error: RmuxError::invalid_target(
                            target.to_string(),
                            "pane identity changed before paste-buffer write",
                        ),
                    },
                )));
            }
            prepare_pane_input_write(&mut state, target, &payload, PaneInputLiveness::RejectDead)
                .map_err(|error| {
                    OrderedPasteBufferResult::Completed(Response::Error(ErrorResponse { error }))
                })?
        };
        write_bytes_to_target_io(write, payload)
            .await
            .map_err(|error| {
                OrderedPasteBufferResult::Completed(Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!(
                        "failed to write buffer to pane {}:{}.{}: {}",
                        session_name, window_index, pane_index, error
                    )),
                }))
            })
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
        let (mut content, line_flags) = {
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
            };
            let line_flags = if request.include_format {
                match state.capture_line_format_flags(&request.target, capture_request) {
                    Ok(flags) => Some(flags),
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            } else {
                None
            };
            let content =
                match state.capture_transcript_for_command(&request.target, capture_request) {
                    Ok(content) => content,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
            (content, line_flags)
        };
        apply_capture_format_flags(&mut content, &request, line_flags.as_deref());

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

fn render_paste_payload(content: &[u8], request: &rmux_proto::PasteBufferRequest) -> Vec<u8> {
    let separator = request
        .separator
        .as_deref()
        .map(str::as_bytes)
        .unwrap_or_else(|| {
            if request.linefeed {
                b"\n".as_slice()
            } else {
                b"\r".as_slice()
            }
        });

    let mut output = Vec::new();
    let mut start = 0;
    while let Some(relative_end) = content[start..].iter().position(|&byte| byte == b'\n') {
        let end = start + relative_end;
        append_paste_chunk(&mut output, &content[start..end], request.raw);
        output.extend_from_slice(separator);
        start = end + 1;
    }
    if start < content.len() {
        append_paste_chunk(&mut output, &content[start..], request.raw);
    }
    output
}

fn bracketed_paste_payload(mut payload: Vec<u8>, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return payload;
    }

    let mut bracketed_payload = Vec::with_capacity(payload.len() + 12);
    bracketed_payload.extend_from_slice(b"\x1b[200~");
    bracketed_payload.append(&mut payload);
    bracketed_payload.extend_from_slice(b"\x1b[201~");
    bracketed_payload
}

fn append_paste_chunk(output: &mut Vec<u8>, chunk: &[u8], raw: bool) {
    if raw {
        output.extend_from_slice(chunk);
    } else {
        output.extend_from_slice(&encode_paste_bytes(chunk));
    }
}
