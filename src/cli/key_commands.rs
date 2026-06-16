use std::path::Path;

use rmux_client::{connect_or_absent, ConnectResult};
use rmux_core::{
    formats::{render_template, FormatContext},
    key_code_lookup_bits, key_string_lookup_string, KeyBindingDisplay, KeyBindingSortOrder,
    KeyBindingStore, KEYC_NONE, KEYC_UNKNOWN, LIST_KEYS_TEMPLATE,
};
use rmux_proto::{
    BindKeyRequest, CommandOutput, ListKeysRequest, SendKeysExt2Request, SendKeysExtRequest,
    UnbindKeyRequest,
};

use super::{
    expect_command_output, resolve_pane_target_spec, run_command, run_command_resolved,
    write_command_output, ExitFailure,
};
use crate::cli_args::{BindKeyArgs, ListKeysArgs, SendKeysArgs, SendPrefixArgs, UnbindKeyArgs};

pub(super) fn run_send_keys(args: SendKeysArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    if args.unsupported_prefix {
        return Err(ExitFailure::new(1, "command send-keys: unknown flag -p"));
    }
    if args.repeat_count == Some(0) {
        return Err(ExitFailure::new(1, "repeat count too small"));
    }

    if send_keys_uses_legacy_path(&args) {
        let target = args
            .target
            .clone()
            .expect("legacy send-keys path requires explicit target");
        return run_command_resolved(socket_path, "send-keys", move |connection| {
            let target = resolve_pane_target_spec(connection, &target)?;
            connection
                .send_keys(target, args.keys)
                .map_err(ExitFailure::from_client)
        });
    }

    run_command_resolved(socket_path, "send-keys", move |connection| {
        let target = args
            .target
            .as_ref()
            .map(|target| resolve_pane_target_spec(connection, target))
            .transpose()?;
        let response = if let Some(target_client) = args.client_target {
            connection.send_keys_extended_target_client(SendKeysExt2Request {
                target,
                keys: args.keys,
                expand_formats: args.expand_formats,
                hex: args.hex,
                literal: args.literal,
                dispatch_key_table: args.key_table,
                copy_mode_command: args.copy_mode,
                forward_mouse_event: args.mouse,
                reset_terminal: args.reset_terminal,
                repeat_count: args.repeat_count,
                target_client: Some(target_client),
            })
        } else {
            connection.send_keys_extended(SendKeysExtRequest {
                target,
                keys: args.keys,
                expand_formats: args.expand_formats,
                hex: args.hex,
                literal: args.literal,
                dispatch_key_table: args.key_table,
                copy_mode_command: args.copy_mode,
                forward_mouse_event: args.mouse,
                reset_terminal: args.reset_terminal,
                repeat_count: args.repeat_count,
            })
        };
        response.map_err(ExitFailure::from_client)
    })
}

fn send_keys_uses_legacy_path(args: &SendKeysArgs) -> bool {
    args.target.is_some()
        && args.client_target.is_none()
        && !args.expand_formats
        && !args.hex
        && !args.literal
        && !args.key_table
        && !args.mouse
        && args.repeat_count.is_none()
        && !args.unsupported_prefix
        && !args.reset_terminal
        && !args.copy_mode
}

pub(super) fn run_bind_key(args: BindKeyArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    run_command(socket_path, "bind-key", move |connection| {
        connection.bind_key(BindKeyRequest {
            table_name: args.table_name(),
            key: args.key,
            note: args.note,
            repeat: args.repeat,
            command: (!args.command.is_empty()).then_some(args.command),
        })
    })
}

pub(super) fn run_unbind_key(args: UnbindKeyArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    run_command(socket_path, "unbind-key", move |connection| {
        connection.unbind_key(UnbindKeyRequest {
            table_name: args.table_name(),
            all: args.all,
            key: args.key,
            quiet: args.quiet,
        })
    })
}

pub(super) fn run_list_keys(args: ListKeysArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let request = ListKeysRequest {
        table_name: args.table_name,
        first_only: args.first_only,
        notes: args.notes,
        include_unnoted: args.include_unnoted,
        reversed: args.reversed,
        format: args.format,
        sort_order: args.sort_order,
        prefix: args.prefix,
        key: args.key,
    };

    match connect_or_absent(socket_path).map_err(ExitFailure::from_client)? {
        ConnectResult::Connected(mut connection) => {
            let response = connection
                .list_keys(request)
                .map_err(ExitFailure::from_client)?;
            let output = expect_command_output(&response, "list-keys")?;
            write_command_output(output)?;
            Ok(0)
        }
        ConnectResult::Absent => run_default_list_keys(request),
    }
}

fn run_default_list_keys(request: ListKeysRequest) -> Result<i32, ExitFailure> {
    let sort_order = match request.sort_order.as_deref() {
        Some(value) => KeyBindingSortOrder::parse(value)
            .ok_or_else(|| ExitFailure::new(1, format!("invalid sort order: {value}")))?,
        None => KeyBindingSortOrder::default(),
    };
    let filter_key = match request.key.as_deref() {
        Some(key) => match key_string_lookup_string(key) {
            Some(key) if key != KEYC_NONE && key != KEYC_UNKNOWN => Some(key_code_lookup_bits(key)),
            _ => return Err(ExitFailure::new(1, format!("unknown key: {key}"))),
        },
        None => None,
    };
    let store = KeyBindingStore::default();
    if let Some(table_name) = request.table_name.as_deref() {
        if store.table(table_name).is_none() {
            return Err(ExitFailure::new(
                1,
                format!("table {table_name} doesn't exist"),
            ));
        }
    }
    let mut bindings = list_default_key_bindings(&store, &request, sort_order);
    if let Some(filter_key) = filter_key {
        bindings.retain(|binding| key_code_lookup_bits(binding.binding().key()) == filter_key);
        if bindings.is_empty() {
            let key = request.key.as_deref().unwrap_or_default();
            return Err(ExitFailure::new(1, format!("unknown key: {key}")));
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
        render_default_list_keys_output(&bindings, &request, render_metrics, notes_key_width);
    write_command_output(&output)?;
    Ok(0)
}

fn list_default_key_bindings(
    store: &KeyBindingStore,
    request: &ListKeysRequest,
    sort_order: KeyBindingSortOrder,
) -> Vec<KeyBindingDisplay> {
    if request.notes && request.table_name.is_none() {
        store
            .list_bindings(None, sort_order, request.reversed)
            .into_iter()
            .filter(|binding| matches!(binding.table_name(), "prefix" | "root"))
            .collect()
    } else {
        store.list_bindings(request.table_name.as_deref(), sort_order, request.reversed)
    }
}

fn render_default_list_keys_output(
    bindings: &[KeyBindingDisplay],
    request: &ListKeysRequest,
    render_metrics: ListKeysRenderMetrics,
    notes_key_width: usize,
) -> CommandOutput {
    let template = request.format.as_deref().unwrap_or(LIST_KEYS_TEMPLATE);
    let note_prefix_width = note_prefix_width(request);
    let lines = bindings
        .iter()
        .map(|binding| {
            if request.format.is_none() && request.notes {
                return render_notes_binding_line(
                    binding,
                    request,
                    "C-b",
                    note_prefix_width,
                    notes_key_width,
                );
            }
            if request.format.is_none() && request.key.is_some() && !request.notes {
                return render_default_key_filtered_binding_line(binding, render_metrics);
            }
            let key_has_repeat = if request.key.is_some() {
                binding.binding().repeat()
            } else {
                render_metrics.has_repeat
            };
            let context = FormatContext::new()
                .with_named_value("key_repeat", bool_format(binding.binding().repeat()))
                .with_named_value("key_note", binding.binding().note().unwrap_or_default())
                .with_named_value(
                    "key_prefix",
                    note_prefix(binding.table_name(), request, "C-b", note_prefix_width),
                )
                .with_named_value("key_table", binding.table_name())
                .with_named_value("key_string", binding.key_string())
                .with_named_value("key_command", binding.command_string())
                .with_named_value("notes_only", bool_format(request.notes))
                .with_named_value("key_has_repeat", bool_format(key_has_repeat))
                .with_named_value(
                    "key_string_width",
                    render_metrics.key_string_width.to_string(),
                )
                .with_named_value(
                    "key_table_width",
                    render_metrics.key_table_width.to_string(),
                );
            render_template(template, &context)
        })
        .collect::<Vec<_>>();
    command_output_from_lines(&lines)
}

fn render_notes_binding_line(
    binding: &KeyBindingDisplay,
    request: &ListKeysRequest,
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

fn render_default_key_filtered_binding_line(
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
            key_string_width: KeyBindingStore::key_string_width(bindings),
            key_table_width: KeyBindingStore::key_table_width(bindings),
            has_repeat: KeyBindingStore::has_repeat(bindings),
        }
    }
}

fn command_output_from_lines(lines: &[String]) -> CommandOutput {
    if lines.is_empty() {
        CommandOutput::from_stdout(Vec::new())
    } else {
        CommandOutput::from_stdout(format!("{}\n", lines.join("\n")).into_bytes())
    }
}

fn bool_format(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn note_prefix_width(request: &ListKeysRequest) -> usize {
    request.prefix.as_deref().map_or("C-b".len() + 1, str::len)
}

fn note_prefix(
    table_name: &str,
    request: &ListKeysRequest,
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

pub(super) fn run_send_prefix(
    args: SendPrefixArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "send-prefix", move |connection| {
        let target = args
            .target
            .as_ref()
            .map(|target| resolve_pane_target_spec(connection, target))
            .transpose()?;
        connection
            .send_prefix(target, args.secondary)
            .map_err(ExitFailure::from_client)
    })
}
