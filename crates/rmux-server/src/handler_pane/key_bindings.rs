use std::path::Path;

use rmux_core::{
    formats::FormatContext, key_code_lookup_bits, key_string_lookup_key, key_string_lookup_string,
    parse_binding_command_tokens_with_parser, KeyBindingDisplay, KeyBindingSortOrder, KEYC_NONE,
    KEYC_UNKNOWN, LIST_KEYS_TEMPLATE,
};
use rmux_proto::{
    BindKeyResponse, CommandOutput, ErrorResponse, ListKeysResponse, OptionName, Response,
    RmuxError, UnbindKeyResponse,
};

use super::{command_output_from_lines, RequestHandler};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::handler::scripting_support::command_parser_from_state;
use crate::pane_terminals::HandlerState;

impl RequestHandler {
    pub(in crate::handler) async fn handle_bind_key(
        &self,
        request: rmux_proto::BindKeyRequest,
    ) -> Response {
        self.handle_bind_key_inner(request, None).await
    }

    pub(in crate::handler) async fn handle_bind_key_for_mode_tree(
        &self,
        request: rmux_proto::BindKeyRequest,
        identity: super::super::mode_tree_support::ModeTreeActionIdentity,
    ) -> Response {
        self.handle_bind_key_inner(request, Some(identity)).await
    }

    async fn handle_bind_key_inner(
        &self,
        request: rmux_proto::BindKeyRequest,
        mode_tree_identity: Option<super::super::mode_tree_support::ModeTreeActionIdentity>,
    ) -> Response {
        let key = match key_string_lookup_string(&request.key) {
            Some(key) if key != KEYC_NONE && key != KEYC_UNKNOWN => key,
            _ => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("unknown key: {}", request.key)),
                });
            }
        };
        let mut state = self.state.lock().await;
        let _mode_tree_guard = if let Some(identity) = mode_tree_identity {
            let active_attach = self.active_attach.lock().await;
            if !identity.matches_active(&state, &active_attach) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("mode-tree is not active".to_owned()),
                });
            }
            Some(active_attach)
        } else {
            None
        };
        let commands = match request.command.as_ref() {
            Some(tokens) => match parse_binding_command_tokens_with_parser(
                &command_parser_from_state(&state),
                tokens,
            ) {
                Ok(commands) => Some(commands),
                Err(error) => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(error.to_string()),
                    });
                }
            },
            None => None,
        };

        let canonical_key = key_string_lookup_key(key_code_lookup_bits(key), false);
        let updated = state.key_bindings.add_binding(
            &request.table_name,
            key,
            request.note,
            request.repeat,
            commands,
        );
        if !updated {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(format!("key is not bound: {canonical_key}")),
            });
        }
        Response::BindKey(BindKeyResponse {
            table_name: request.table_name,
            key: canonical_key,
        })
    }

    pub(in crate::handler) async fn handle_unbind_key(
        &self,
        request: rmux_proto::UnbindKeyRequest,
    ) -> Response {
        self.handle_unbind_key_inner(request, None).await
    }

    pub(in crate::handler) async fn handle_unbind_key_for_mode_tree(
        &self,
        request: rmux_proto::UnbindKeyRequest,
        identity: super::super::mode_tree_support::ModeTreeActionIdentity,
    ) -> Response {
        self.handle_unbind_key_inner(request, Some(identity)).await
    }

    async fn handle_unbind_key_inner(
        &self,
        request: rmux_proto::UnbindKeyRequest,
        mode_tree_identity: Option<super::super::mode_tree_support::ModeTreeActionIdentity>,
    ) -> Response {
        if request.all && request.key.is_some() {
            return unbind_quiet_response_or_error(&request, "key given with -a");
        }
        if !request.all && request.key.is_none() {
            return unbind_quiet_response_or_error(&request, "missing key");
        }

        let mut state = self.state.lock().await;
        let _mode_tree_guard = if let Some(identity) = mode_tree_identity {
            let active_attach = self.active_attach.lock().await;
            if !identity.matches_active(&state, &active_attach) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("mode-tree is not active".to_owned()),
                });
            }
            Some(active_attach)
        } else {
            None
        };
        if state.key_bindings.table(&request.table_name).is_none() {
            return unbind_quiet_response_or_error(
                &request,
                format!("table {} doesn't exist", request.table_name),
            );
        }

        if request.all {
            let removed = state.key_bindings.remove_table(&request.table_name);
            return Response::UnbindKey(UnbindKeyResponse {
                table_name: request.table_name,
                key: None,
                removed,
                all: true,
            });
        }

        let key_string = request
            .key
            .as_deref()
            .expect("validated missing key for unbind-key");
        let key = match key_string_lookup_string(key_string) {
            Some(key) if key != KEYC_NONE && key != KEYC_UNKNOWN => key,
            _ => {
                return unbind_quiet_response_or_error(
                    &request,
                    format!("unknown key: {key_string}"),
                )
            }
        };
        let canonical_key = key_string_lookup_key(key_code_lookup_bits(key), false);
        let removed = state.key_bindings.remove_binding(&request.table_name, key);
        Response::UnbindKey(UnbindKeyResponse {
            table_name: request.table_name,
            key: Some(canonical_key),
            removed,
            all: false,
        })
    }

    pub(in crate::handler) async fn reset_key_binding_for_mode_tree(
        &self,
        table_name: &str,
        key: rmux_core::KeyCode,
        identity: super::super::mode_tree_support::ModeTreeActionIdentity,
    ) -> Result<(), RmuxError> {
        let mut state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        if !identity.matches_active(&state, &active_attach) {
            return Err(RmuxError::Server("mode-tree is not active".to_owned()));
        }
        state.key_bindings.reset_binding(table_name, key);
        Ok(())
    }

    pub(in crate::handler) async fn handle_list_keys(
        &self,
        request: rmux_proto::ListKeysRequest,
    ) -> Response {
        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        if let Some(table_name) = request.table_name.as_deref() {
            if state.key_bindings.table(table_name).is_none() {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("table {table_name} doesn't exist")),
                });
            }
        }
        let sort_order = match request.sort_order.as_deref() {
            Some(value) => match KeyBindingSortOrder::parse(value) {
                Some(value) => value,
                None => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(rmux_core::INVALID_SORT_ORDER.to_owned()),
                    });
                }
            },
            None => KeyBindingSortOrder::default(),
        };
        let filter_key = match request.key.as_deref() {
            Some(key) => match key_string_lookup_string(key) {
                Some(key) if key != KEYC_NONE && key != KEYC_UNKNOWN => {
                    Some(key_code_lookup_bits(key))
                }
                _ => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(format!("invalid key: {key}")),
                    });
                }
            },
            None => None,
        };
        let mut bindings = list_key_bindings(&state, &request, sort_order);
        if let Some(filter_key) = filter_key {
            if request.table_name.is_some() {
                return Response::ListKeys(ListKeysResponse {
                    match_count: 0,
                    output: command_output_from_lines(&[]),
                });
            }
            bindings.retain(|binding| key_code_lookup_bits(binding.binding().key()) == filter_key);
        }
        if request.notes && !request.include_unnoted {
            bindings.retain(|binding| binding.binding().note().is_some());
        }
        let render_metrics = ListKeysRenderMetrics::from_bindings(&bindings);
        let notes_key_width = list_keys_notes_key_width(&bindings);
        if request.first_only {
            bindings.truncate(1);
        }

        let output = render_list_keys_output(
            &state,
            &socket_path,
            &bindings,
            &request,
            render_metrics,
            notes_key_width,
        );
        Response::ListKeys(ListKeysResponse {
            match_count: bindings.len(),
            output,
        })
    }
}

fn unbind_quiet_response_or_error(
    request: &rmux_proto::UnbindKeyRequest,
    message: impl Into<String>,
) -> Response {
    if request.quiet {
        Response::UnbindKey(UnbindKeyResponse {
            table_name: request.table_name.clone(),
            key: request.key.clone(),
            removed: false,
            all: request.all,
        })
    } else {
        Response::Error(ErrorResponse {
            error: RmuxError::Server(message.into()),
        })
    }
}

fn list_key_bindings(
    state: &HandlerState,
    request: &rmux_proto::ListKeysRequest,
    sort_order: KeyBindingSortOrder,
) -> Vec<KeyBindingDisplay> {
    let reversed = request.reversed && request.sort_order.is_some();
    if request.notes && request.table_name.is_none() {
        state
            .key_bindings
            .list_bindings(None, sort_order, reversed)
            .into_iter()
            .filter(|binding| matches!(binding.table_name(), "prefix" | "root"))
            .collect()
    } else {
        state
            .key_bindings
            .list_bindings(request.table_name.as_deref(), sort_order, reversed)
    }
}

fn render_list_keys_output(
    state: &HandlerState,
    socket_path: &Path,
    bindings: &[KeyBindingDisplay],
    request: &rmux_proto::ListKeysRequest,
    render_metrics: ListKeysRenderMetrics,
    notes_key_width: usize,
) -> CommandOutput {
    let template = request.format.as_deref().unwrap_or(LIST_KEYS_TEMPLATE);
    let effective_prefix = state
        .options
        .global_value(OptionName::Prefix)
        .or_else(|| state.options.resolve(None, OptionName::Prefix))
        .unwrap_or("C-b");
    let note_prefix_width = note_prefix_width(request, effective_prefix);
    let lines = bindings
        .iter()
        .map(|binding| {
            let default_template = request.format.is_none();
            if request.format.is_none() && request.notes {
                return render_notes_binding_line(
                    binding,
                    request,
                    effective_prefix,
                    note_prefix_width,
                    notes_key_width,
                );
            }
            let key_string = if default_template {
                list_keys_command_key(binding.key_string())
            } else {
                binding.key_string().to_owned()
            };
            let key_string_width = if default_template {
                render_metrics.escaped_key_string_width
            } else {
                render_metrics.key_string_width
            };
            let key_has_repeat = if request.key.is_some() {
                binding.binding().repeat()
            } else {
                render_metrics.has_repeat
            };
            let context = RuntimeFormatContext::new(FormatContext::new())
                .with_state(state)
                .with_socket_path(socket_path)
                .with_named_value("key_repeat", bool_string(binding.binding().repeat()))
                .with_named_value("key_note", binding.binding().note().unwrap_or_default())
                .with_named_value(
                    "key_prefix",
                    note_prefix(
                        binding.table_name(),
                        request,
                        effective_prefix,
                        note_prefix_width,
                    ),
                )
                .with_named_value("key_table", binding.table_name())
                .with_named_value("key_string", key_string)
                .with_named_value("key_command", binding.command_string())
                .with_named_value("notes_only", bool_string(request.notes))
                .with_named_value("key_has_repeat", bool_string(key_has_repeat))
                .with_named_value("key_string_width", key_string_width.to_string())
                .with_named_value(
                    "key_table_width",
                    render_metrics.key_table_width.to_string(),
                );
            render_runtime_template(template, &context, false)
        })
        .collect::<Vec<_>>();
    command_output_from_lines(&lines)
}

fn render_notes_binding_line(
    binding: &KeyBindingDisplay,
    request: &rmux_proto::ListKeysRequest,
    effective_prefix: &str,
    note_prefix_width: usize,
    key_width: usize,
) -> String {
    let prefix = note_prefix(
        binding.table_name(),
        request,
        effective_prefix,
        note_prefix_width,
    );
    let key = list_keys_note_key(binding.key_string());
    format!(
        "{prefix}{key:<key_width$} {note}",
        note = binding.binding().note().unwrap_or_default()
    )
}

fn list_keys_notes_key_width(bindings: &[KeyBindingDisplay]) -> usize {
    bindings
        .iter()
        .map(|binding| list_keys_note_key(binding.key_string()).len())
        .max()
        .unwrap_or(0)
}

fn list_keys_note_key(key: &str) -> &str {
    key.strip_prefix('\\')
        .or_else(|| {
            key.strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(key)
}

fn list_keys_command_key(key: &str) -> String {
    let mut escaped = String::new();
    for ch in key.chars() {
        if matches!(ch, '"' | '#' | '$' | '%' | '\'' | ';' | '{' | '}' | '~') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ListKeysRenderMetrics {
    key_string_width: usize,
    escaped_key_string_width: usize,
    key_table_width: usize,
    has_repeat: bool,
}

impl ListKeysRenderMetrics {
    fn from_bindings(bindings: &[KeyBindingDisplay]) -> Self {
        Self {
            key_string_width: rmux_core::KeyBindingStore::key_string_width(bindings),
            escaped_key_string_width: bindings
                .iter()
                .map(|binding| list_keys_command_key(binding.key_string()).len())
                .max()
                .unwrap_or(0),
            key_table_width: rmux_core::KeyBindingStore::key_table_width(bindings),
            has_repeat: rmux_core::KeyBindingStore::has_repeat(bindings),
        }
    }
}

fn bool_string(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn note_prefix_width(request: &rmux_proto::ListKeysRequest, effective_prefix: &str) -> usize {
    request
        .prefix
        .as_deref()
        .map_or(effective_prefix.len() + 1, str::len)
}

fn note_prefix(
    table_name: &str,
    request: &rmux_proto::ListKeysRequest,
    effective_prefix: &str,
    width: usize,
) -> String {
    if !request.notes {
        return request.prefix.clone().unwrap_or_default();
    }

    if table_name != "prefix" {
        return " ".repeat(width);
    }

    request
        .prefix
        .clone()
        .unwrap_or_else(|| format!("{effective_prefix} "))
}
