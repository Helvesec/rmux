use rmux_core::{key_code_lookup_bits, key_string_lookup_key, key_string_lookup_string};
use rmux_proto::{
    ErrorResponse, OptionName, PaneTarget, Response, RmuxError, SendKeysResponse,
    SendPrefixResponse,
};

use super::super::RequestHandler;
use super::{
    encode_key_for_target, encode_mouse_for_target, encode_tokens_for_target,
    expand_send_key_tokens, prepare_pane_input_write, prepare_synchronized_pane_input_writes,
    resolve_input_target, write_bytes_to_targets, PaneInputWrite,
};
use crate::keys::{parse_key_code, resolve_hex_key};

impl RequestHandler {
    pub(in crate::handler) async fn handle_send_keys(
        &self,
        request: rmux_proto::SendKeysRequest,
    ) -> Response {
        let key_count = request.keys.len();
        let keys = if request.keys.is_empty() {
            request.keys
        } else {
            match self
                .consume_copy_mode_key_tokens(std::process::id(), &request.target, &request.keys)
                .await
            {
                Ok(keys) => keys,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };
        if key_count > 0 && keys.is_empty() {
            return Response::SendKeys(SendKeysResponse { key_count });
        }
        let prepared = {
            let state = self.state.lock().await;
            let resolved = match encode_tokens_for_target(&state, &request.target, &keys) {
                Ok(resolved) => resolved,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let writes =
                match prepare_synchronized_pane_input_writes(&state, &request.target, &resolved) {
                    Ok(writes) => writes,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
            (writes, resolved)
        };
        self.write_pane_input_and_mark_interactive(prepared.0, prepared.1, key_count)
            .await
    }

    #[async_recursion::async_recursion]
    pub(in crate::handler) async fn handle_send_keys_ext(
        &self,
        requester_pid: u32,
        request: rmux_proto::SendKeysExtRequest,
    ) -> Response {
        self.handle_send_keys_ext_inner(requester_pid, request, None)
            .await
    }

    pub(in crate::handler) async fn handle_send_keys_ext2(
        &self,
        requester_pid: u32,
        request: rmux_proto::SendKeysExt2Request,
    ) -> Response {
        let target_client = request.target_client;
        let request = rmux_proto::SendKeysExtRequest {
            target: request.target,
            keys: request.keys,
            expand_formats: request.expand_formats,
            hex: request.hex,
            literal: request.literal,
            dispatch_key_table: request.dispatch_key_table,
            copy_mode_command: request.copy_mode_command,
            forward_mouse_event: request.forward_mouse_event,
            reset_terminal: request.reset_terminal,
            repeat_count: request.repeat_count,
        };
        self.handle_send_keys_ext_inner(requester_pid, request, target_client)
            .await
    }

    #[async_recursion::async_recursion]
    async fn handle_send_keys_ext_inner(
        &self,
        requester_pid: u32,
        request: rmux_proto::SendKeysExtRequest,
        target_client: Option<String>,
    ) -> Response {
        let target_attach_pid = match target_client.as_deref() {
            Some(target_client) => match self
                .find_target_attach_client_pid(requester_pid, target_client, "send-keys")
                .await
            {
                Ok(Some(attach_pid)) => Some(attach_pid),
                Ok(None) => {
                    return Response::SendKeys(SendKeysResponse { key_count: 0 });
                }
                Err(error) => return Response::Error(ErrorResponse { error }),
            },
            None => None,
        };
        let attached_session = {
            let active_attach = self.active_attach.lock().await;
            active_attach.current_session_candidate(target_attach_pid.unwrap_or(requester_pid))
        };
        let target = {
            let state = self.state.lock().await;
            match resolve_input_target(&state, request.target.as_ref(), attached_session.as_ref()) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        let tokens = {
            let state = self.state.lock().await;
            match expand_send_key_tokens(&state, &target, &request.keys, request.expand_formats) {
                Ok(tokens) => tokens,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        if request.copy_mode_command {
            return Box::pin(self.handle_send_keys_copy_mode(
                target_attach_pid.unwrap_or(requester_pid),
                &request,
                target,
                &tokens,
            ))
            .await;
        }

        if request.dispatch_key_table {
            let attach_pid = match target_attach_pid {
                Some(attach_pid) => attach_pid,
                None => match self
                    .resolve_attached_client_pid(requester_pid, "send-keys")
                    .await
                {
                    Ok(attach_pid) => attach_pid,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                },
            };
            let effective_requester_pid = target_attach_pid.unwrap_or(requester_pid);
            let repeat_count = request.repeat_count.unwrap_or(1).max(1);
            for token in &tokens {
                let key = if request.hex {
                    resolve_hex_key(token).map(u64::from)
                } else {
                    parse_key_code(token)
                };
                let Some(key) = key else {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(format!("unknown key: {token}")),
                    });
                };
                for _ in 0..repeat_count {
                    if let Err(error) = Box::pin(self.dispatch_attached_key(
                        attach_pid,
                        effective_requester_pid,
                        &target,
                        key,
                    ))
                    .await
                    {
                        return Response::Error(ErrorResponse { error });
                    }
                }
            }
            return Response::SendKeys(SendKeysResponse {
                key_count: tokens.len(),
            });
        }

        if request.forward_mouse_event {
            match self.exit_clock_mode(&target).await {
                Ok(true) => {
                    return Response::SendKeys(SendKeysResponse { key_count: 0 });
                }
                Ok(false) => {}
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
            let attach_pid = match target_attach_pid {
                Some(attach_pid) => attach_pid,
                None => match self
                    .resolve_attached_client_pid(requester_pid, "send-keys")
                    .await
                {
                    Ok(attach_pid) => attach_pid,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                },
            };
            let mouse_event = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .and_then(|active| active.mouse.current_event.clone())
            };
            let Some(mouse_event) = mouse_event else {
                return Response::SendKeys(SendKeysResponse { key_count: 0 });
            };
            let state = self.state.lock().await;
            let bytes = match encode_mouse_for_target(&state, &target, &mouse_event) {
                Ok(bytes) => bytes,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let write = match prepare_pane_input_write(&state, &target, &bytes) {
                Ok(write) => write,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            drop(state);
            return self
                .write_pane_input_and_mark_interactive(vec![write], bytes, 0)
                .await;
        }

        let tokens =
            if !tokens.is_empty() && !request.hex && !request.literal && !request.reset_terminal {
                match self
                    .consume_copy_mode_key_tokens(
                        target_attach_pid.unwrap_or(requester_pid),
                        &target,
                        &tokens,
                    )
                    .await
                {
                    Ok(tokens) => tokens,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            } else {
                tokens
            };
        if !request.keys.is_empty() && tokens.is_empty() {
            return Response::SendKeys(SendKeysResponse {
                key_count: request.keys.len(),
            });
        }

        let prepared = {
            let state = self.state.lock().await;
            if request.reset_terminal {
                if let Err(error) = state.reset_pane_terminal_state(&target) {
                    return Response::Error(ErrorResponse { error });
                }
            }
            let mut bytes = Vec::new();
            if request.hex {
                for token in &tokens {
                    let Some(byte) = resolve_hex_key(token) else {
                        return Response::Error(ErrorResponse {
                            error: RmuxError::Server(format!("invalid hex byte: {token}")),
                        });
                    };
                    bytes.push(byte);
                }
            } else if request.literal {
                for token in &tokens {
                    bytes.extend_from_slice(token.as_bytes());
                }
            } else {
                match encode_tokens_for_target(&state, &target, &tokens) {
                    Ok(encoded) => bytes.extend_from_slice(&encoded),
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            }
            let repeat_count = request.repeat_count.unwrap_or(1).max(1);
            let repeated = bytes.repeat(repeat_count);
            let writes = match prepare_synchronized_pane_input_writes(&state, &target, &repeated) {
                Ok(writes) => writes,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            (writes, repeated)
        };
        self.write_pane_input_and_mark_interactive(prepared.0, prepared.1, request.keys.len())
            .await
    }

    async fn consume_copy_mode_key_tokens(
        &self,
        requester_pid: u32,
        target: &PaneTarget,
        tokens: &[String],
    ) -> Result<Vec<String>, RmuxError> {
        let mut remaining = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            if !self.target_is_in_copy_mode(target).await? {
                remaining.extend(tokens[index..].iter().cloned());
                break;
            }
            let Some(key) = parse_key_code(token) else {
                remaining.extend(tokens[index..].iter().cloned());
                break;
            };
            let handled = self
                .handle_detached_copy_mode_key_code(requester_pid, target.clone(), key)
                .await?;
            if !handled {
                remaining.extend(tokens[index..].iter().cloned());
                break;
            }
        }
        Ok(remaining)
    }

    pub(in crate::handler) async fn handle_send_prefix(
        &self,
        requester_pid: u32,
        request: rmux_proto::SendPrefixRequest,
    ) -> Response {
        let attached_session = {
            let active_attach = self.active_attach.lock().await;
            active_attach.current_session_candidate(requester_pid)
        };
        let target = {
            let state = self.state.lock().await;
            match resolve_input_target(&state, request.target.as_ref(), attached_session.as_ref()) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        let (writes, encoded, canonical_key) = {
            let state = self.state.lock().await;
            let option = if request.secondary {
                OptionName::Prefix2
            } else {
                OptionName::Prefix
            };
            let Some(value) = state.options.resolve(Some(target.session_name()), option) else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("prefix key is not configured".to_owned()),
                });
            };
            let Some(key) = key_string_lookup_string(value) else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("unknown key: {value}")),
                });
            };
            let canonical_key = key_string_lookup_key(key_code_lookup_bits(key), false);
            let encoded = match encode_key_for_target(&state, &target, key) {
                Ok(Some(encoded)) => encoded,
                Ok(None) => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(format!(
                            "key {} cannot be sent to a pane",
                            canonical_key
                        )),
                    });
                }
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let writes = match prepare_synchronized_pane_input_writes(&state, &target, &encoded) {
                Ok(writes) => writes,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            (writes, encoded, canonical_key)
        };

        match self
            .write_pane_input_and_mark_interactive(writes, encoded, 1)
            .await
        {
            Response::SendKeys(_) => Response::SendPrefix(SendPrefixResponse {
                target: Some(target),
                key: canonical_key,
                key_count: 1,
            }),
            other => other,
        }
    }

    async fn write_pane_input_and_mark_interactive(
        &self,
        writes: Vec<PaneInputWrite>,
        bytes: Vec<u8>,
        key_count: usize,
    ) -> Response {
        let wrote_bytes = !bytes.is_empty();
        let sessions = input_write_sessions(&writes);
        let response = write_bytes_to_targets(writes, bytes, key_count).await;
        if wrote_bytes && matches!(response, Response::SendKeys(_)) {
            for session_name in sessions {
                self.mark_attached_session_interactive_input(&session_name)
                    .await;
            }
        }
        response
    }
}

fn input_write_sessions(writes: &[PaneInputWrite]) -> Vec<rmux_proto::SessionName> {
    let mut sessions = Vec::new();
    for write in writes {
        let session_name = write.session_name();
        if !sessions.iter().any(|existing| existing == session_name) {
            sessions.push(session_name.clone());
        }
    }
    sessions
}
