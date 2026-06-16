use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rmux_core::{encode_paste_bytes, GridRenderOptions, LifecycleEvent, ScreenCaptureRange};
use rmux_proto::{
    CapturePaneResponse, ClearHistoryResponse, CommandOutput, DeleteBufferResponse, ErrorResponse,
    ListBuffersResponse, LoadBufferResponse, OptionName, PasteBufferResponse, Response, RmuxError,
    SaveBufferResponse, SetBufferResponse, ShowBufferResponse,
};

use super::pane_support::write_bracketed_pane_payload;
use super::RequestHandler;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, PaneCaptureRequest};

#[path = "handler_buffer/list.rs"]
mod list;

use list::{
    command_output_from_lines, render_list_buffer_line, sort_buffer_entries, BufferSortOrder,
};

static SAVE_BUFFER_TEMP_ID: AtomicU64 = AtomicU64::new(0);
impl RequestHandler {
    pub(super) async fn handle_set_buffer(
        &self,
        requester_pid: u32,
        request: rmux_proto::SetBufferRequest,
    ) -> Response {
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

        let content = if request.append {
            self.append_buffer_content(request.name.as_deref(), request.content)
                .await
        } else {
            Ok(request.content)
        };

        match content {
            Ok(content) => {
                let clipboard_bytes = request.set_clipboard.then_some(content.clone());
                match self.store_buffer(request.name, content).await {
                    Ok(buffer_name) => {
                        if let Some(bytes) = clipboard_bytes.as_deref() {
                            self.copy_bytes_to_attached_clipboard(
                                requester_pid,
                                "set-buffer",
                                bytes,
                            )
                            .await;
                        }
                        Response::SetBuffer(SetBufferResponse { buffer_name })
                    }
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
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
        let session_name = request.target.session_name().clone();
        let window_index = request.target.window_index();
        let pane_index = request.target.pane_index();

        let (buffer_name, content, buffer_order, master, bracketed_mode) = {
            let mut state = self.state.lock().await;

            if !state.sessions.contains_session(&session_name) {
                return Response::Error(ErrorResponse {
                    error: session_not_found(&session_name),
                });
            }

            let (name, content, order) =
                match state.buffers.show_with_order(request.name.as_deref()) {
                    Ok((name, content, order)) => (name.to_owned(), content.to_vec(), order),
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };

            let pane = match state
                .sessions
                .session(&session_name)
                .and_then(|session| session.window_at(window_index))
                .and_then(|window| window.pane(pane_index))
            {
                Some(pane) => pane,
                None => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::invalid_target(
                            format!("{session_name}:{window_index}.{pane_index}"),
                            "pane index does not exist in session",
                        ),
                    })
                }
            };
            let bracketed_mode = request.bracketed
                && state
                    .pane_screen_state(&session_name, pane.id())
                    .is_some_and(|state| {
                        state.mode & rmux_core::input::mode::MODE_BRACKETPASTE != 0
                    });

            let master =
                match state.clone_pane_master_if_alive(&session_name, window_index, pane_index) {
                    Ok(master) => master,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };

            (name, content, order, master, bracketed_mode)
        };

        let payload = render_paste_payload(&content, &request);
        if let Err(error) = write_bracketed_pane_payload(master, payload, bracketed_mode).await {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(format!(
                    "failed to write buffer to pane {}:{}.{}: {}",
                    session_name, window_index, pane_index, error
                )),
            });
        }

        if request.delete_after {
            self.pause_before_paste_buffer_delete().await;
            let mut state = self.state.lock().await;
            let deleted = state
                .buffers
                .delete_if_order_matches(&buffer_name, buffer_order);
            drop(state);
            if deleted {
                self.emit(LifecycleEvent::PasteBufferDeleted {
                    buffer_name: buffer_name.clone(),
                })
                .await;
            }
        }

        Response::PasteBuffer(PasteBufferResponse { buffer_name })
    }

    pub(super) async fn handle_list_buffers(
        &self,
        request: rmux_proto::ListBuffersRequest,
    ) -> Response {
        let state = self.state.lock().await;
        let sort_order = match BufferSortOrder::parse(request.sort_order.as_deref()) {
            Some(order) => order,
            None => {
                let value = request.sort_order.unwrap_or_default();
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("invalid sort order: {value}")),
                });
            }
        };

        let mut entries = state.buffers.entries();
        sort_buffer_entries(&mut entries, sort_order, request.reversed);
        let lines = entries
            .into_iter()
            .filter_map(|entry| render_list_buffer_line(&state, &request, entry))
            .collect::<Vec<_>>();

        Response::ListBuffers(ListBuffersResponse {
            output: command_output_from_lines(&lines),
        })
    }

    pub(super) async fn handle_delete_buffer(
        &self,
        request: rmux_proto::DeleteBufferRequest,
    ) -> Response {
        let mut state = self.state.lock().await;

        match state.buffers.delete(request.name.as_deref()) {
            Ok(buffer_name) => {
                drop(state);
                self.emit(LifecycleEvent::PasteBufferDeleted {
                    buffer_name: buffer_name.clone(),
                })
                .await;
                Response::DeleteBuffer(DeleteBufferResponse { buffer_name })
            }
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_load_buffer(
        &self,
        requester_pid: u32,
        request: rmux_proto::LoadBufferRequest,
    ) -> Response {
        let resolved_path = resolve_buffer_path(&request.path, request.cwd.as_deref());
        let content = match fs::read(&resolved_path) {
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
                    self.copy_bytes_to_attached_clipboard(requester_pid, "load-buffer", bytes)
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
        let save_result = if request.append {
            append_buffer_to_path(&resolved_path, &content)
        } else {
            save_buffer_atomically(&resolved_path, &content)
        };
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
        let content = {
            let mut state = self.state.lock().await;
            let range = ScreenCaptureRange {
                start: request.start,
                end: request.end,
                start_is_absolute: request.start_is_absolute,
                end_is_absolute: request.end_is_absolute,
            };
            let options = capture_render_options(&request);
            match state.capture_transcript_for_command(
                &request.target,
                PaneCaptureRequest {
                    range,
                    options,
                    alternate: request.alternate,
                    use_mode_screen: request.use_mode_screen,
                    pending_input: request.pending_input,
                    quiet: request.quiet,
                    escape_pending: request.escape_sequences,
                },
            ) {
                Ok(content) => content,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

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

    async fn append_buffer_content(
        &self,
        name: Option<&str>,
        mut content: Vec<u8>,
    ) -> Result<Vec<u8>, RmuxError> {
        let state = self.state.lock().await;
        if let Some(name) = name {
            if let Some(existing) = state.buffers.get(name) {
                let mut combined = Vec::with_capacity(existing.len() + content.len());
                combined.extend_from_slice(existing);
                combined.append(&mut content);
                return Ok(combined);
            }
        }
        Ok(content)
    }

    async fn rename_buffer(
        &self,
        old_name: Option<String>,
        new_name: String,
    ) -> Result<String, RmuxError> {
        let mut state = self.state.lock().await;
        let outcome = state.buffers.rename(old_name.as_deref(), &new_name)?;
        drop(state);

        if outcome.changed() {
            if outcome.replaced() {
                self.emit(LifecycleEvent::PasteBufferDeleted {
                    buffer_name: outcome.new_name().to_owned(),
                })
                .await;
            }
            self.emit(LifecycleEvent::PasteBufferDeleted {
                buffer_name: outcome.old_name().to_owned(),
            })
            .await;
            self.emit(LifecycleEvent::PasteBufferChanged {
                buffer_name: outcome.new_name().to_owned(),
            })
            .await;
        }

        Ok(outcome.new_name().to_owned())
    }

    async fn store_buffer(
        &self,
        name: Option<String>,
        content: Vec<u8>,
    ) -> Result<String, RmuxError> {
        let mut state = self.state.lock().await;
        let buffer_limit = parse_buffer_limit(&state);
        let outcome = state.buffers.set(name.as_deref(), content, buffer_limit)?;
        let buffer_name = outcome.buffer_name().map(str::to_owned).unwrap_or_default();
        drop(state);

        for evicted in outcome.evicted() {
            self.emit(LifecycleEvent::PasteBufferDeleted {
                buffer_name: evicted.clone(),
            })
            .await;
        }
        if !buffer_name.is_empty() {
            self.emit(LifecycleEvent::PasteBufferChanged {
                buffer_name: buffer_name.clone(),
            })
            .await;
        }

        Ok(buffer_name)
    }

    async fn copy_bytes_to_attached_clipboard(
        &self,
        requester_pid: u32,
        command_name: &str,
        bytes: &[u8],
    ) {
        let Some((attach_pid, terminal_context)) = self
            .clipboard_attach_for_requester(requester_pid, command_name)
            .await
        else {
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
            .send_attach_control(
                attach_pid,
                AttachControl::Write(payload),
                command_name,
                None,
            )
            .await;
    }
}

fn capture_render_options(request: &rmux_proto::CapturePaneRequest) -> GridRenderOptions {
    let join_wrapped = request.join_wrapped;
    GridRenderOptions {
        join_wrapped,
        with_sequences: request.escape_ansi,
        escape_sequences: request.escape_sequences,
        include_empty_cells: !join_wrapped && !request.preserve_trailing_spaces,
        use_tmux_cell_capacity: request.do_not_trim_spaces,
        trim_spaces: !join_wrapped && !request.do_not_trim_spaces,
    }
}

fn parse_buffer_limit(state: &crate::pane_terminals::HandlerState) -> usize {
    state
        .options
        .resolve(None, OptionName::BufferLimit)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50)
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

fn save_buffer_atomically(destination: &Path, content: &[u8]) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let (mut temp_file, temp_path) = create_save_buffer_temp_file(parent, destination)?;

    let write_result = (|| {
        temp_file.write_all(content)?;
        temp_file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    if let Err(error) = fs::rename(&temp_path, destination) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    Ok(())
}

fn append_buffer_to_path(destination: &Path, content: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(destination)?;
    file.write_all(content)
}

fn create_save_buffer_temp_file(
    parent: &Path,
    destination: &Path,
) -> io::Result<(std::fs::File, PathBuf)> {
    let destination_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "buffer".to_owned());

    for _ in 0..128 {
        let temp_id = SAVE_BUFFER_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            ".{destination_name}.rmux-save-buffer-{temp_id:016x}.tmp"
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((file, temp_path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "failed to allocate a temporary file alongside '{}'",
            destination.display()
        ),
    ))
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

fn append_paste_chunk(output: &mut Vec<u8>, chunk: &[u8], raw: bool) {
    if raw {
        output.extend_from_slice(chunk);
    } else {
        output.extend_from_slice(&encode_paste_bytes(chunk));
    }
}
