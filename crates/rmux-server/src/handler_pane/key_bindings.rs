use rmux_core::{
    formats::FormatContext, key_code_lookup_bits, key_string_lookup_key, key_string_lookup_string,
    parse_binding_command_tokens, KeyBindingDisplay, KeyBindingSortOrder, KEYC_NONE, KEYC_UNKNOWN,
    LIST_KEYS_TEMPLATE,
};
use rmux_proto::{
    BindKeyResponse, CommandOutput, ErrorResponse, ListKeysResponse, OptionName, Response,
    RmuxError, UnbindKeyResponse,
};

use super::{command_output_from_lines, RequestHandler};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

impl RequestHandler {
    pub(in crate::handler) async fn handle_bind_key(
        &self,
        request: rmux_proto::BindKeyRequest,
    ) -> Response {
        let key = match key_string_lookup_string(&request.key) {
            Some(key) if key != KEYC_NONE && key != KEYC_UNKNOWN => key,
            _ => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("unknown key: {}", request.key)),
                });
            }
        };
        let commands = match request.command.as_ref() {
            Some(tokens) => match parse_binding_command_tokens(tokens) {
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
        let mut state = self.state.lock().await;
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
        if request.all && request.key.is_some() {
            return unbind_quiet_response_or_error(&request, "key given with -a");
        }
        if !request.all && request.key.is_none() {
            return unbind_quiet_response_or_error(&request, "missing key");
        }

        let mut state = self.state.lock().await;
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

    pub(in crate::handler) async fn handle_list_keys(
        &self,
        request: rmux_proto::ListKeysRequest,
    ) -> Response {
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
                        error: RmuxError::Server(format!("invalid sort order: {value}")),
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
                        error: RmuxError::Server(format!("unknown key: {key}")),
                    });
                }
            },
            None => None,
        };
        let mut bindings = list_key_bindings(&state, &request, sort_order);
        if let Some(filter_key) = filter_key {
            bindings.retain(|binding| key_code_lookup_bits(binding.binding().key()) == filter_key);
            if bindings.is_empty() {
                let key = request.key.as_deref().unwrap_or_default();
                return Response::Error(ErrorResponse {
                    error: RmuxError::Message(format!("unknown key: {key}")),
                });
            }
        }
        if request.notes && !request.include_unnoted {
            bindings.retain(|binding| binding.binding().note().is_some());
        }
        let render_metrics = ListKeysRenderMetrics::from_bindings(&bindings);
        let notes_key_width = list_keys_notes_key_width(&bindings);
        if request.first_only {
            bindings.truncate(1);
        }

        let output =
            render_list_keys_output(&state, &bindings, &request, render_metrics, notes_key_width);
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
    if request.notes && request.table_name.is_none() {
        state
            .key_bindings
            .list_bindings(None, sort_order, request.reversed)
            .into_iter()
            .filter(|binding| matches!(binding.table_name(), "prefix" | "root"))
            .collect()
    } else {
        state.key_bindings.list_bindings(
            request.table_name.as_deref(),
            sort_order,
            request.reversed,
        )
    }
}

fn render_list_keys_output(
    state: &HandlerState,
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
            if request.format.is_none() && request.notes {
                return render_notes_binding_line(
                    binding,
                    request,
                    effective_prefix,
                    note_prefix_width,
                    notes_key_width,
                );
            }
            if request.format.is_none() && request.key.is_some() && !request.notes {
                return render_key_filtered_binding_line(binding, render_metrics);
            }
            let key_has_repeat = if request.key.is_some() {
                binding.binding().repeat()
            } else {
                render_metrics.has_repeat
            };
            let context = RuntimeFormatContext::new(FormatContext::new())
                .with_state(state)
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
                .with_named_value("key_string", binding.key_string())
                .with_named_value("key_command", binding.command_string())
                .with_named_value("notes_only", bool_string(request.notes))
                .with_named_value("key_has_repeat", bool_string(key_has_repeat))
                .with_named_value(
                    "key_string_width",
                    render_metrics.key_string_width.to_string(),
                )
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

fn render_key_filtered_binding_line(
    binding: &KeyBindingDisplay,
    render_metrics: ListKeysRenderMetrics,
) -> String {
    let repeat = if binding.binding().repeat() {
        " -r"
    } else {
        ""
    };
    format!(
        "bind-key{repeat} -T {table:<table_width$} {key:<key_width$} {command}",
        table = binding.table_name(),
        table_width = render_metrics.key_table_width,
        key = binding.key_string(),
        key_width = render_metrics.key_string_width,
        command = binding.command_string()
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ListKeysRenderMetrics {
    key_string_width: usize,
    key_table_width: usize,
    has_repeat: bool,
}

impl ListKeysRenderMetrics {
    fn from_bindings(bindings: &[KeyBindingDisplay]) -> Self {
        Self {
            key_string_width: rmux_core::KeyBindingStore::key_string_width(bindings),
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
