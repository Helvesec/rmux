use crate::{PaneId, Result, RmuxError, SessionId, WindowId};

use super::super::target::parse_error;
use super::{ListedPane, ListedSession, LiveDetails};

pub(crate) fn parse_details_line(line: &str) -> Result<LiveDetails> {
    if line.is_empty() {
        return Ok(LiveDetails::default());
    }
    // The trailing field is `#{pane_start_path}`, which is a filesystem
    // path. Tabs in such a path are valid bytes on Unix, so the parser
    // anchors the leading separators with `splitn` and treats the
    // remainder as the path verbatim instead of dropping characters past
    // an embedded tab.
    let fields: Vec<&str> = line.splitn(18, '\t').collect();
    if fields.len() < 18 {
        return Ok(LiveDetails::default());
    }

    Ok(LiveDetails {
        pane_id: parse_optional_pane_id(fields[0])?,
        pid: parse_optional_u32(fields[1]),
        dead: parse_truthy_flag(fields[2]),
        dead_status: parse_optional_i32(fields[3]),
        dead_signal: parse_optional_i32(fields[4]),
        cols: parse_optional_u16(fields[5]).unwrap_or(0),
        rows: parse_optional_u16(fields[6]).unwrap_or(0),
        cursor_x: parse_optional_u16(fields[7]).unwrap_or(0),
        cursor_y: parse_optional_u16(fields[8]).unwrap_or(0),
        cursor_visible: parse_truthy_flag_default(fields[9], true),
        cursor_style: parse_optional_u32(fields[10]).unwrap_or(0),
        history_bytes: parse_optional_u64(fields[11]).unwrap_or(0),
        history_size: parse_optional_u64(fields[12]).unwrap_or(0),
        start_command: decode_command_field(fields[13])?,
        generation: parse_optional_u64(fields[14]).unwrap_or(0),
        lifecycle_revision: parse_optional_u64(fields[15]).unwrap_or(0),
        output_sequence: parse_optional_u64(fields[16]).unwrap_or(0),
        current_path: optional_string(fields[17]),
    })
}

pub(super) fn parse_session_line(line: &str) -> Result<ListedSession> {
    let mut fields = line.split('\t');
    let name = fields
        .next()
        .ok_or_else(|| parse_error("session info line omitted name"))?;
    let id = fields
        .next()
        .ok_or_else(|| parse_error("session info line omitted id"))?;
    if fields.next().is_some() {
        return Err(parse_error("session info line had trailing fields"));
    }
    Ok(ListedSession {
        name: rmux_proto::SessionName::new(name).map_err(RmuxError::protocol)?,
        id: parse_session_id(id)?,
    })
}

pub(super) fn parse_pane_list_line(line: &str) -> Result<ListedPane> {
    let mut fields = line.split(':');
    let window_index = fields
        .next()
        .ok_or_else(|| parse_error("pane list line omitted window index"))?;
    let pane_index = fields
        .next()
        .ok_or_else(|| parse_error("pane list line omitted pane index"))?;
    let pane_id = fields
        .next()
        .ok_or_else(|| parse_error("pane list line omitted pane id"))?;
    if fields.next().is_some() {
        return Err(parse_error("pane list line had trailing fields"));
    }

    let window_index = parse_u32(window_index, "pane list window index")?;
    Ok(ListedPane {
        window_index,
        pane_index: parse_u32(pane_index, "pane index")?,
        pane_id: parse_pane_id(pane_id)?,
    })
}

fn parse_session_id(value: &str) -> Result<SessionId> {
    parse_prefixed_u32(value, '$', "session id").map(SessionId::new)
}

pub(super) fn parse_window_id(value: &str) -> Result<WindowId> {
    parse_prefixed_u32(value, '@', "window id").map(WindowId::new)
}

pub(super) fn parse_pane_id(value: &str) -> Result<PaneId> {
    parse_prefixed_u32(value, '%', "pane id").map(PaneId::new)
}

fn parse_optional_pane_id(value: &str) -> Result<Option<PaneId>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_pane_id(value).map(Some)
    }
}

fn parse_prefixed_u32(value: &str, prefix: char, field: &str) -> Result<u32> {
    let raw = value
        .strip_prefix(prefix)
        .ok_or_else(|| parse_error(format!("{field} `{value}` omitted `{prefix}` prefix")))?;
    parse_u32(raw, field)
}

fn parse_u32(value: &str, field: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|error| parse_error(format!("invalid {field} `{value}`: {error}")))
}

fn parse_truthy_flag(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

fn parse_truthy_flag_default(value: &str, default: bool) -> bool {
    if value.is_empty() {
        default
    } else {
        value != "0"
    }
}

fn parse_optional_u16(value: &str) -> Option<u16> {
    if value.is_empty() {
        None
    } else {
        value.parse::<u16>().ok()
    }
}

fn parse_optional_u32(value: &str) -> Option<u32> {
    if value.is_empty() {
        None
    } else {
        value.parse::<u32>().ok()
    }
}

fn parse_optional_u64(value: &str) -> Option<u64> {
    if value.is_empty() {
        None
    } else {
        value.parse::<u64>().ok()
    }
}

fn parse_optional_i32(value: &str) -> Option<i32> {
    if value.is_empty() {
        None
    } else {
        value.parse::<i32>().ok()
    }
}

fn optional_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn decode_command_field(value: &str) -> Result<Option<Vec<String>>> {
    if value.is_empty() {
        return Ok(None);
    }
    let mut command = value
        .split('\x1f')
        .map(percent_decode_string)
        .collect::<Result<Vec<_>>>()?;
    if command.len() == 1 {
        command[0] = unquote_tmux_shell_command(&command[0]);
    }
    Ok(Some(command))
}

fn unquote_tmux_shell_command(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return value.to_owned();
    };
    let mut unquoted = String::with_capacity(inner.len());
    let mut escaped = false;
    for character in inner.chars() {
        if escaped {
            unquoted.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            unquoted.push(character);
        }
    }
    if escaped {
        unquoted.push('\\');
    }
    unquoted
}

fn percent_decode_string(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(parse_error("truncated percent escape in pane command"));
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|error| parse_error(format!("pane command was not utf-8: {error}")))
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(parse_error(format!(
            "invalid percent escape digit `{}` in pane command",
            char::from(byte)
        ))),
    }
}
