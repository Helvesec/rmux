use std::path::Path;

use rmux_proto::{PaneBroadcastInputRequest, Response};
use serde_json::{json, Value};

use crate::cli_args::{
    BroadcastKeysArgs, ExpectPaneArgs, FindPanesArgs, FindSessionsArgs, LocatorArgs,
};
use crate::cli_response::tmux_cli_error_message;

use super::super::{list_session_names, resolve_pane_target_spec, ExitFailure};
use super::common::{
    connect_cli, find_visible_text, matches_json, pane_snapshot, resolve_pane_ref,
    stable_pane_ref_for_slot, write_json, write_stdout_bytes, write_stdout_line, SCHEMA_VERSION,
};

const FIND_PANES_FIELD_SEPARATOR: char = '\u{1f}';
const FIND_PANES_RECORD_SEPARATOR: char = '\u{1e}';
const FIND_PANES_FORMAT: &str = "#{session_name}\u{1f}#{window_index}\u{1f}#{pane_index}\u{1f}#{pane_id}\u{1f}#{pane_title}\u{1f}#{pane_current_command}\u{1f}#{pane_current_path}\u{1e}";

pub(crate) fn run_locator(args: LocatorArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let mut connection = connect_cli(socket_path)?;
    let target = resolve_pane_ref(&mut connection, args.target.as_ref(), "locator")?;
    let snapshot = pane_snapshot(&mut connection, target)?;
    let matches = find_visible_text(&snapshot, &args.get_by_text);
    if args.json {
        return write_json(&json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "locator": "get-by-text",
            "matches": matches_json(&matches),
            "count": matches.len(),
        }));
    }
    let lines = matches
        .iter()
        .map(|found| format!("{}:{}:{}", found.row, found.col, found.text))
        .collect::<Vec<_>>()
        .join("\n");
    if lines.is_empty() {
        return write_stdout_bytes(b"");
    }
    write_stdout_line(&lines)
}

pub(crate) fn run_expect_pane(
    args: ExpectPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect_cli(socket_path)?;
    let target = resolve_pane_ref(&mut connection, args.target.as_ref(), "expect-pane")?;
    let snapshot = pane_snapshot(&mut connection, target)?;
    let matches = find_visible_text(&snapshot, &args.get_by_text);
    let ok = if args.visible {
        !matches.is_empty()
    } else if args.hidden {
        matches.is_empty()
    } else {
        matches.len() == args.count.unwrap_or_default()
    };
    let assertion = if args.visible {
        "visible"
    } else if args.hidden {
        "hidden"
    } else {
        "count"
    };
    if args.json {
        let exit_code = if ok { 0 } else { 1 };
        write_json(&json!({
            "schema_version": SCHEMA_VERSION,
            "ok": ok,
            "assertion": assertion,
            "locator": "get-by-text",
            "expected_count": args.count,
            "count": matches.len(),
            "matches": matches_json(&matches),
        }))?;
        return Ok(exit_code);
    }
    if ok {
        return Ok(0);
    }
    Err(ExitFailure::new(
        1,
        format!(
            "expect-pane failed: get-by-text {:?} assertion {assertion} saw {} matches",
            args.get_by_text,
            matches.len()
        ),
    ))
}

pub(crate) fn run_find_panes(args: FindPanesArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let mut connection = connect_cli(socket_path)?;
    let sessions = list_session_names(&mut connection)?;
    let mut panes = Vec::new();
    for session_name in sessions {
        let response = connection
            .list_panes_in_window(session_name, None, Some(FIND_PANES_FORMAT.to_owned()))
            .map_err(ExitFailure::from_client)?;
        let Response::ListPanes(response) = response else {
            continue;
        };
        let text = String::from_utf8_lossy(response.output.stdout());
        panes.extend(
            text.split(FIND_PANES_RECORD_SEPARATOR)
                .filter_map(parse_pane_row),
        );
    }
    panes.retain(|pane| pane_matches(pane, &args));
    if args.json {
        return write_json(&json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "panes": Value::Array(
                panes
                    .iter()
                    .map(|pane| {
                        json!({
                            "session_name": pane.session_name,
                            "window_index": pane.window_index,
                            "pane_index": pane.pane_index,
                            "pane_id": pane.pane_id,
                            "title": pane.title,
                            "current_command": pane.current_command,
                            "cwd": pane.cwd,
                        })
                    })
                    .collect(),
            ),
        }));
    }
    let lines = panes
        .iter()
        .map(|pane| {
            format!(
                "{}\t{}:{}.{}\t{}\t{}\t{}",
                pane.pane_id,
                pane.session_name,
                pane.window_index,
                pane.pane_index,
                pane.title,
                pane.current_command,
                pane.cwd
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if lines.is_empty() {
        return write_stdout_bytes(b"");
    }
    write_stdout_line(&lines)
}

pub(crate) fn run_find_sessions(
    args: FindSessionsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect_cli(socket_path)?;
    let mut sessions = list_session_names(&mut connection)?
        .into_iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    sessions.retain(|name| {
        args.name.as_deref().is_none_or(|expected| name == expected)
            && args
                .name_prefix
                .as_deref()
                .is_none_or(|prefix| name.starts_with(prefix))
    });
    if args.json {
        return write_json(&json!({
            "schema_version": SCHEMA_VERSION,
            "ok": true,
            "sessions": Value::Array(
                sessions
                    .iter()
                    .map(|name| json!({ "session_name": name }))
                    .collect(),
            ),
        }));
    }
    let lines = sessions.join("\n");
    if lines.is_empty() {
        return write_stdout_bytes(b"");
    }
    write_stdout_line(&lines)
}

pub(crate) fn run_broadcast_keys(
    args: BroadcastKeysArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect_cli(socket_path)?;
    let mut targets = Vec::with_capacity(args.targets.len());
    for target in &args.targets {
        let slot = resolve_pane_target_spec(&mut connection, target)?;
        targets.push(stable_pane_ref_for_slot(
            &mut connection,
            &slot,
            "broadcast-keys",
        )?);
    }
    match connection
        .pane_broadcast_input(PaneBroadcastInputRequest {
            targets,
            keys: args.keys,
            literal: args.literal,
        })
        .map_err(ExitFailure::from_client)?
    {
        Response::PaneBroadcastInput(response) if response.failures.is_empty() => Ok(0),
        Response::PaneBroadcastInput(response) => Err(ExitFailure::new(
            1,
            format!(
                "broadcast-keys failed for {} of {} targets",
                response.failures.len(),
                response.successes.len() + response.failures.len()
            ),
        )),
        Response::Error(error) => Err(ExitFailure::new(
            1,
            tmux_cli_error_message("broadcast-keys", &error.error),
        )),
        other => Err(ExitFailure::new(
            1,
            format!(
                "protocol error: unexpected '{}' response for broadcast-keys",
                other.command_name()
            ),
        )),
    }
}

struct PaneRow {
    session_name: String,
    window_index: String,
    pane_index: String,
    pane_id: String,
    title: String,
    current_command: String,
    cwd: String,
}

fn parse_pane_row(line: &str) -> Option<PaneRow> {
    let line = line.trim_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }
    let fields = line.split(FIND_PANES_FIELD_SEPARATOR).collect::<Vec<_>>();
    let [session_name, window_index, pane_index, pane_id, title, current_command, cwd] =
        fields.as_slice()
    else {
        return None;
    };
    Some(PaneRow {
        session_name: (*session_name).to_owned(),
        window_index: (*window_index).to_owned(),
        pane_index: (*pane_index).to_owned(),
        pane_id: (*pane_id).to_owned(),
        title: (*title).to_owned(),
        current_command: (*current_command).to_owned(),
        cwd: (*cwd).to_owned(),
    })
}

fn pane_matches(pane: &PaneRow, args: &FindPanesArgs) -> bool {
    args.title
        .as_deref()
        .is_none_or(|title| pane.title == title)
        && args
            .title_prefix
            .as_deref()
            .is_none_or(|prefix| pane.title.starts_with(prefix))
        && args
            .current_command
            .as_deref()
            .is_none_or(|command| pane.current_command == command)
        && args.cwd.as_deref().is_none_or(|cwd| pane.cwd == cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pane_row_ignores_record_newlines() {
        let line = "\nalpha\u{1f}0\u{1f}2\u{1f}%1\u{1f}title\u{1f}bash\u{1f}/tmp/project\n";

        let row = parse_pane_row(line).expect("row parses");

        assert_eq!(row.session_name, "alpha");
        assert_eq!(row.window_index, "0");
        assert_eq!(row.pane_index, "2");
        assert_eq!(row.pane_id, "%1");
        assert_eq!(row.title, "title");
        assert_eq!(row.current_command, "bash");
        assert_eq!(row.cwd, "/tmp/project");
    }
}
