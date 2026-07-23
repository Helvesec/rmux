use std::io::{self, ErrorKind, Write};

use rmux_core::formats::is_truthy;
use rmux_proto::{CommandOutput, ListClientsResponse, ListWindowsResponse};
use serde_json::{json, Map, Value};

use super::ExitFailure;

const FIELD_SEPARATOR: char = '\x1f';
const FIELD_SEPARATOR_STR: &str = "\x1f";
const ROW_SEPARATOR: char = '\x1e';
const ROW_SEPARATOR_STR: &str = "\x1e";

const LIST_CLIENTS_FIELDS: &[JsonField] = &[
    JsonField::string("client_name"),
    JsonField::string("client_session"),
    JsonField::string("client_tty"),
    JsonField::number("client_width"),
    JsonField::number("client_height"),
    JsonField::number("client_pid"),
    JsonField::bool("client_readonly"),
    JsonField::bool("client_control_mode"),
    JsonField::bool("client_utf8"),
];

const LIST_PANES_FIELDS: &[JsonField] = &[
    JsonField::string("session_name"),
    JsonField::number("window_index"),
    JsonField::number("pane_index"),
    JsonField::string("pane_id"),
    JsonField::number("pane_width"),
    JsonField::number("pane_height"),
    JsonField::bool("pane_active"),
    JsonField::bool("pane_dead"),
    JsonField::string("pane_current_command"),
    JsonField::string("pane_current_path"),
];

const LIST_SESSIONS_FIELDS: &[JsonField] = &[
    JsonField::string("session_id"),
    JsonField::string("session_name"),
    JsonField::number("session_windows"),
    JsonField::bool("session_attached"),
    JsonField::bool("session_grouped"),
    JsonField::string("session_group"),
    JsonField::number("session_created"),
];

const LIST_WINDOWS_JSON_FIELDS: &[JsonField] = &[
    JsonField::string("session_name"),
    JsonField::number("window_index"),
    JsonField::string("window_id"),
    JsonField::string("window_name"),
    JsonField::number("window_panes"),
    JsonField::number("window_width"),
    JsonField::number("window_height"),
    JsonField::string("window_layout"),
    JsonField::bool("window_active"),
    JsonField::bool("window_last_flag"),
];

pub(super) fn list_clients_json_format() -> String {
    json_format(LIST_CLIENTS_FIELDS)
}

pub(super) fn list_panes_json_format() -> String {
    json_format(LIST_PANES_FIELDS)
}

pub(super) fn list_sessions_json_format() -> String {
    json_format(LIST_SESSIONS_FIELDS)
}

pub(super) fn list_windows_json_format() -> String {
    LIST_WINDOWS_JSON_FIELDS
        .iter()
        .map(|field| format!("#{{n:{0}}}:#{{{0}}}", field.name))
        .collect::<Vec<_>>()
        .join("")
}

fn json_format(fields: &[JsonField]) -> String {
    let mut format = fields
        .iter()
        .map(|field| format!("#{{{}}}", field.name))
        .collect::<Vec<_>>()
        .join(FIELD_SEPARATOR_STR);
    format.push_str(ROW_SEPARATOR_STR);
    format
}

#[derive(Clone, Copy)]
struct JsonField {
    name: &'static str,
    kind: JsonFieldKind,
}

impl JsonField {
    const fn bool(name: &'static str) -> Self {
        Self {
            name,
            kind: JsonFieldKind::Bool,
        }
    }

    const fn number(name: &'static str) -> Self {
        Self {
            name,
            kind: JsonFieldKind::Number,
        }
    }

    const fn string(name: &'static str) -> Self {
        Self {
            name,
            kind: JsonFieldKind::String,
        }
    }
}

#[derive(Clone, Copy)]
enum JsonFieldKind {
    Bool,
    Number,
    String,
}

pub(super) fn write_list_clients_json(response: &ListClientsResponse) -> Result<i32, ExitFailure> {
    write_delimited_output_as_json(
        response.command_output(),
        LIST_CLIENTS_FIELDS,
        "list-clients",
    )
}

pub(super) fn write_list_panes_json(output: &CommandOutput) -> Result<i32, ExitFailure> {
    write_delimited_output_as_json(output, LIST_PANES_FIELDS, "list-panes")
}

pub(super) fn write_list_sessions_json(output: &CommandOutput) -> Result<i32, ExitFailure> {
    write_delimited_output_as_json(output, LIST_SESSIONS_FIELDS, "list-sessions")
}

pub(super) fn write_length_prefixed_list_windows_json(
    output: &CommandOutput,
) -> Result<i32, ExitFailure> {
    let rows =
        parse_length_prefixed_rows(output.stdout(), LIST_WINDOWS_JSON_FIELDS, "list-windows")?;
    write_json_value(&Value::Array(rows), "list-windows")
}

pub(super) fn filter_delimited_json_output(
    output: &CommandOutput,
    command_name: &'static str,
) -> Result<CommandOutput, ExitFailure> {
    let stdout = stdout_string(output.stdout(), command_name)?;
    let mut filtered = String::new();
    for record in split_delimited_records(&stdout) {
        let Some((filter_value, rendered_record)) = record.split_once(FIELD_SEPARATOR) else {
            return Err(ExitFailure::new(
                1,
                format!("{command_name} filter output missing separator"),
            ));
        };
        if is_truthy(filter_value) {
            filtered.push_str(rendered_record);
            filtered.push(ROW_SEPARATOR);
            filtered.push('\n');
        }
    }
    Ok(CommandOutput::from_stdout(filtered.into_bytes()))
}

pub(super) fn write_list_windows_json(response: &ListWindowsResponse) -> Result<i32, ExitFailure> {
    let rows = response
        .windows
        .iter()
        .map(|window| {
            json!({
                "session_name": window.target.session_name().to_string(),
                "window_index": window.target.window_index(),
                "window_id": window.window_id,
                "window_name": window.name.as_deref().unwrap_or(""),
                "window_panes": window.pane_count,
                "window_width": window.size.cols,
                "window_height": window.size.rows,
                "window_layout": window.layout.to_string(),
                "window_active": window.active,
                "window_last_flag": window.last,
            })
        })
        .collect::<Vec<_>>();

    write_json_value(&Value::Array(rows), "list-windows")
}

fn write_delimited_output_as_json(
    output: &CommandOutput,
    fields: &[JsonField],
    command_name: &'static str,
) -> Result<i32, ExitFailure> {
    let stdout = stdout_string(output.stdout(), command_name)?;
    let rows = parse_delimited_rows(&stdout, fields, command_name)?;
    write_json_value(&Value::Array(rows), command_name)
}

fn parse_delimited_rows(
    stdout: &str,
    fields: &[JsonField],
    command_name: &'static str,
) -> Result<Vec<Value>, ExitFailure> {
    if !stdout.contains(ROW_SEPARATOR) {
        return stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| parse_delimited_row(line, fields, command_name))
            .collect();
    }

    split_delimited_records(stdout)
        .map(|record| parse_delimited_row(record, fields, command_name))
        .collect()
}

fn split_delimited_records(stdout: &str) -> impl Iterator<Item = &str> {
    stdout
        .split(ROW_SEPARATOR)
        .enumerate()
        .filter_map(|(index, record)| {
            let record = if index == 0 {
                record
            } else {
                record.strip_prefix('\n').unwrap_or(record)
            };
            (!record.is_empty()).then_some(record)
        })
}

fn parse_delimited_row(
    line: &str,
    fields: &[JsonField],
    command_name: &'static str,
) -> Result<Value, ExitFailure> {
    let parts = line.split(FIELD_SEPARATOR).collect::<Vec<_>>();
    if parts.len() != fields.len() {
        return Err(ExitFailure::new(
            1,
            format!(
                "{command_name} --json expected {} fields, got {}",
                fields.len(),
                parts.len()
            ),
        ));
    }

    let mut object = Map::with_capacity(fields.len());
    for (field, value) in fields.iter().zip(parts) {
        object.insert(
            field.name.to_owned(),
            field_value(field, value, command_name)?,
        );
    }
    Ok(Value::Object(object))
}

fn parse_length_prefixed_rows(
    bytes: &[u8],
    fields: &[JsonField],
    command_name: &'static str,
) -> Result<Vec<Value>, ExitFailure> {
    let mut rows = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let mut object = Map::with_capacity(fields.len());
        for field in fields {
            let relative_colon = bytes[cursor..]
                .iter()
                .position(|byte| *byte == b':')
                .ok_or_else(|| length_prefix_error(command_name, field.name))?;
            let colon = cursor + relative_colon;
            let length = std::str::from_utf8(&bytes[cursor..colon])
                .ok()
                .filter(|value| !value.is_empty())
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| length_prefix_error(command_name, field.name))?;
            let value_start = colon + 1;
            let value_end = value_start.checked_add(length).ok_or_else(|| {
                ExitFailure::new(1, format!("{command_name} --json field length overflow"))
            })?;
            let value = bytes.get(value_start..value_end).ok_or_else(|| {
                ExitFailure::new(
                    1,
                    format!(
                        "{command_name} --json field '{}' exceeds the captured row",
                        field.name
                    ),
                )
            })?;
            let value = std::str::from_utf8(value).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "{command_name} --json field '{}' is not UTF-8: {error}",
                        field.name
                    ),
                )
            })?;
            object.insert(
                field.name.to_owned(),
                typed_field_value(field, value, command_name)?,
            );
            cursor = value_end;
        }
        if bytes.get(cursor) != Some(&b'\n') {
            return Err(ExitFailure::new(
                1,
                format!("{command_name} --json row is missing its terminator"),
            ));
        }
        cursor += 1;
        rows.push(Value::Object(object));
    }
    Ok(rows)
}

fn length_prefix_error(command_name: &'static str, field_name: &'static str) -> ExitFailure {
    ExitFailure::new(
        1,
        format!("{command_name} --json field '{field_name}' has an invalid length prefix"),
    )
}

fn field_value(
    field: &JsonField,
    value: &str,
    command_name: &'static str,
) -> Result<Value, ExitFailure> {
    if value.contains(FIELD_SEPARATOR_STR) || value.contains(ROW_SEPARATOR_STR) {
        return Err(ExitFailure::new(
            1,
            format!(
                "{command_name} --json field '{}' contains an internal separator",
                field.name
            ),
        ));
    }

    typed_field_value(field, value, command_name)
}

fn typed_field_value(
    field: &JsonField,
    value: &str,
    command_name: &'static str,
) -> Result<Value, ExitFailure> {
    match field.kind {
        JsonFieldKind::String => Ok(Value::String(value.to_owned())),
        JsonFieldKind::Bool => parse_bool(value, field.name, command_name).map(Value::Bool),
        JsonFieldKind::Number => parse_number(value, field.name, command_name),
    }
}

fn parse_bool(
    value: &str,
    field_name: &'static str,
    command_name: &'static str,
) -> Result<bool, ExitFailure> {
    match value {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" | "" => Ok(false),
        other => Err(ExitFailure::new(
            1,
            format!("{command_name} --json field '{field_name}' is not boolean: {other}"),
        )),
    }
}

fn parse_number(
    value: &str,
    field_name: &'static str,
    command_name: &'static str,
) -> Result<Value, ExitFailure> {
    if value.is_empty() {
        return Ok(Value::Null);
    }
    let number = value.parse::<i64>().map_err(|error| {
        ExitFailure::new(
            1,
            format!("{command_name} --json field '{field_name}' is not numeric: {error}"),
        )
    })?;
    Ok(Value::Number(number.into()))
}

pub(super) fn write_json_object(
    value: &serde_json::Value,
    command_name: &'static str,
) -> Result<i32, ExitFailure> {
    write_json_value(value, command_name)
}

pub(super) fn stdout_string(
    bytes: &[u8],
    command_name: &'static str,
) -> Result<String, ExitFailure> {
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        ExitFailure::new(
            1,
            format!("{command_name} --json received non-UTF-8 output: {error}"),
        )
    })
}

fn write_json_value(value: &Value, command_name: &'static str) -> Result<i32, ExitFailure> {
    let mut stdout = io::stdout().lock();
    match serde_json::to_writer(&mut stdout, value) {
        Ok(()) => Ok(0),
        Err(error) if error.is_io() && error.io_error_kind() == Some(ErrorKind::BrokenPipe) => {
            Ok(0)
        }
        Err(error) => Err(ExitFailure::new(
            1,
            format!("failed to write {command_name} JSON: {error}"),
        )),
    }?;
    match stdout.write_all(b"\n") {
        Ok(()) => Ok(0),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(0),
        Err(error) => Err(ExitFailure::new(
            1,
            format!("failed to write {command_name} JSON: {error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        list_sessions_json_format, list_windows_json_format, parse_delimited_rows,
        parse_length_prefixed_rows, LIST_PANES_FIELDS, LIST_SESSIONS_FIELDS,
        LIST_WINDOWS_JSON_FIELDS,
    };

    #[test]
    fn list_sessions_format_uses_internal_separator() {
        assert_eq!(
            list_sessions_json_format(),
            "#{session_id}\x1f#{session_name}\x1f#{session_windows}\x1f#{session_attached}\x1f#{session_grouped}\x1f#{session_group}\x1f#{session_created}\x1e"
        );
    }

    #[test]
    fn parses_typed_rows() {
        let rows = parse_delimited_rows(
            "$1\x1fdemo\x1f2\x1f1\x1f0\x1f\x1f123\x1e\n",
            LIST_SESSIONS_FIELDS,
            "list-sessions",
        )
        .expect("row parses");

        assert_eq!(rows[0]["session_name"], "demo");
        assert_eq!(rows[0]["session_windows"], 2);
        assert_eq!(rows[0]["session_attached"], true);
        assert_eq!(rows[0]["session_grouped"], false);
    }

    #[test]
    fn parses_string_fields_containing_newlines() {
        let rows = parse_delimited_rows(
            "$1\x1fdemo\nwith-newline\x1f2\x1f1\x1f0\x1f\x1f123\x1e\n",
            LIST_SESSIONS_FIELDS,
            "list-sessions",
        )
        .expect("row parses");

        assert_eq!(rows[0]["session_name"], "demo\nwith-newline");
    }

    #[test]
    fn rejects_malformed_rows() {
        let error = parse_delimited_rows("demo\x1f0\x1e\n", LIST_PANES_FIELDS, "list-panes")
            .expect_err("field count mismatch should fail");

        assert!(error.message().contains("expected 10 fields"));
    }

    #[test]
    fn list_windows_length_prefix_preserves_all_user_control_characters() {
        assert!(list_windows_json_format()
            .starts_with("#{n:session_name}:#{session_name}#{n:window_index}:#{window_index}"));
        let values = [
            "alpha",
            "0",
            "@1",
            "unit\x1frow\x1e\n雪",
            "1",
            "80",
            "24",
            "tiled",
            "1",
            "0",
        ];
        let mut payload = Vec::new();
        for value in values {
            payload.extend_from_slice(format!("{}:", value.len()).as_bytes());
            payload.extend_from_slice(value.as_bytes());
        }
        payload.push(b'\n');

        let rows = parse_length_prefixed_rows(&payload, LIST_WINDOWS_JSON_FIELDS, "list-windows")
            .expect("length-prefixed row parses");
        assert_eq!(rows[0]["window_name"], "unit\x1frow\x1e\n雪");
        assert_eq!(rows[0]["window_index"], 0);
        assert_eq!(rows[0]["window_active"], true);
    }
}
