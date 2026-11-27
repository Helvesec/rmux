use std::io::{self, ErrorKind, Write};

use rmux_proto::{CommandOutput, ListClientsResponse};
use serde_json::{Map, Value};

use super::ExitFailure;

const FIELD_SEPARATOR: char = '\x1f';
const FIELD_SEPARATOR_STR: &str = "\x1f";
const ROW_SEPARATOR: char = '\x1e';
const ROW_SEPARATOR_STR: &str = "\x1e";

// In pane/window rows every literal US is escaped before the two delimiters
// are inserted. Each format variable is therefore read exactly once.
const ESCAPED_US: &str = "\x1fe";
const FIELD_END: &str = "\x1ff";
const ROW_TERMINATOR: &str = "\x1f\n";

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
    escaped_row_format(LIST_PANES_FIELDS)
}

pub(super) fn list_sessions_json_format() -> String {
    json_format(LIST_SESSIONS_FIELDS)
}

pub(super) fn list_windows_json_format() -> String {
    escaped_row_format(LIST_WINDOWS_JSON_FIELDS)
}

fn escaped_row_format(fields: &[JsonField]) -> String {
    let fields = fields
        .iter()
        .map(|field| format!("#{{s/{FIELD_SEPARATOR}/{ESCAPED_US}/:{}}}", field.name))
        .collect::<Vec<_>>()
        .join(FIELD_END);
    // The list renderer supplies the newline after each row.
    format!("{fields}{FIELD_SEPARATOR}")
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
    let rows = parse_escaped_rows(output.stdout(), LIST_PANES_FIELDS, "list-panes")?;
    write_json_value(&Value::Array(rows), "list-panes")
}

pub(super) fn write_list_sessions_json(output: &CommandOutput) -> Result<i32, ExitFailure> {
    write_delimited_output_as_json(output, LIST_SESSIONS_FIELDS, "list-sessions")
}

pub(super) fn write_list_windows_json(output: &CommandOutput) -> Result<i32, ExitFailure> {
    let rows = parse_escaped_rows(output.stdout(), LIST_WINDOWS_JSON_FIELDS, "list-windows")?;
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

fn parse_escaped_rows(
    bytes: &[u8],
    fields: &[JsonField],
    command_name: &'static str,
) -> Result<Vec<Value>, ExitFailure> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ExitFailure::new(
            1,
            format!("{command_name} --json received non-UTF-8 output: {error}"),
        )
    })?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let rows = text.strip_suffix(ROW_TERMINATOR).ok_or_else(|| {
        ExitFailure::new(
            1,
            format!("{command_name} --json row is missing its terminator"),
        )
    })?;
    rows.split(ROW_TERMINATOR)
        .map(|row| {
            let mut values = row.split(FIELD_END);
            let mut object = Map::with_capacity(fields.len());
            let wrong_field_count = || {
                ExitFailure::new(
                    1,
                    format!("{command_name} --json expected {} fields", fields.len()),
                )
            };
            for field in fields {
                let raw = values.next().ok_or_else(wrong_field_count)?;
                let value = raw.replace(ESCAPED_US, FIELD_SEPARATOR_STR);
                object.insert(
                    field.name.to_owned(),
                    typed_field_value(field, &value, command_name)?,
                );
            }
            if values.next().is_some() {
                return Err(wrong_field_count());
            }
            Ok(Value::Object(object))
        })
        .collect()
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
#[path = "json_output/tests"]
mod tests {
    use super::{
        list_sessions_json_format, parse_delimited_rows, LIST_PANES_FIELDS, LIST_SESSIONS_FIELDS,
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

    mod framing;
}
