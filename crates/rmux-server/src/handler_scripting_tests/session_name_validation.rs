use crate::handler::scripting_support::parse_request_from_parts;
use rmux_core::{ManagedSessionStore, OptionStore, TargetFindContext};
use rmux_proto::{RmuxError, SessionName, Target, TerminalSize};

#[test]
fn scripting_session_name_consumers_preserve_empty_diagnostic() {
    let name = SessionName::new("alpha").unwrap();
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let context = TargetFindContext::new(Some(Target::Session(name)));
    // Each parser family that consumes parse_session_name, including both
    // source and replacement names and compact-flag sibling paths.
    for (command, arguments) in [
        ("new-session", vec!["-s", ""]),
        ("new-session", vec!["-ds", ""]),
        ("new-session", vec!["-t", ""]),
        ("new-session", vec!["-dt", ""]),
        ("has-session", vec!["-t", ""]),
        ("kill-session", vec!["-t", ""]),
        ("rename-session", vec!["-t", "", "replacement"]),
        ("rename-session", vec![""]),
        ("attach-session", vec!["-t", ""]),
        ("detach-client", vec!["-s", ""]),
        ("detach-client", vec!["-as", ""]),
        ("list-clients", vec!["-t", ""]),
        ("list-clients", vec!["-rt", ""]),
        ("list-windows", vec!["-t", ""]),
        ("list-windows", vec!["-rt", ""]),
        ("set-environment", vec!["-t", "", "KEY", "value"]),
        ("set-environment", vec!["-ht", "", "KEY", "value"]),
        ("show-environment", vec!["-t", ""]),
    ] {
        let error = parse_request_from_parts(
            command.to_owned(),
            arguments.into_iter().map(str::to_owned).collect(),
            None,
            &sessions,
            &OptionStore::default(),
            &context,
        )
        .unwrap_err();
        assert_eq!(error, RmuxError::EmptySessionName, "{command}");
        assert_eq!(
            error.to_string(),
            "invalid session name: session names must be non-empty"
        );
    }
}

#[test]
fn scripting_session_targets_read_the_stored_name_of_a_backslash_session() {
    // `new -s 'b\c'` stores `b\\c`; tmux 3.7b finds it from a config or
    // `source-file` line spelled `-t 'b\\c'`. Each target-session consumer
    // must carry that stored name, never the doubly escaped `b\\\\c`.
    let stored = SessionName::new("b\\c").unwrap();
    assert_eq!(stored.as_str(), "b\\\\c");
    let doubly_escaped = SessionName::new(stored.as_str()).unwrap();
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(stored.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let context = TargetFindContext::new(Some(Target::Session(stored.clone())));
    let typed = stored.as_str();
    let window = format!("{typed}:0");
    for (command, arguments) in [
        ("new-session", vec!["-d", "-s", "grp", "-t", typed]),
        ("new-session", vec!["-dt", typed, "-s", "grp"]),
        ("has-session", vec!["-t", typed]),
        ("kill-session", vec!["-t", typed]),
        ("rename-session", vec!["-t", typed, "replacement"]),
        ("attach-session", vec!["-t", typed]),
        ("detach-client", vec!["-s", typed]),
        ("detach-client", vec!["-as", typed]),
        ("list-clients", vec!["-t", typed]),
        ("list-clients", vec!["-rt", typed]),
        ("list-windows", vec!["-t", typed]),
        ("list-windows", vec!["-rt", typed]),
        ("set-environment", vec!["-t", typed, "KEY", "value"]),
        ("set-environment", vec!["-ht", typed, "KEY", "value"]),
        ("show-environment", vec!["-t", typed]),
        ("select-window", vec!["-t", window.as_str()]),
    ] {
        let request = parse_request_from_parts(
            command.to_owned(),
            arguments.iter().map(|value| (*value).to_owned()).collect(),
            None,
            &sessions,
            &OptionStore::default(),
            &context,
        )
        .unwrap_or_else(|error| panic!("{command} {arguments:?}: {error}"));
        let names = request_strings(&request);
        assert!(
            names.iter().any(|name| name == stored.as_str()),
            "{command} {arguments:?} lost the stored name: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == doubly_escaped.as_str()),
            "{command} {arguments:?} escaped the target twice: {names:?}"
        );
    }
}

fn request_strings(request: &rmux_proto::Request) -> Vec<String> {
    fn collect(value: &serde_json::Value, strings: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => strings.push(text.clone()),
            serde_json::Value::Array(values) => {
                values.iter().for_each(|value| collect(value, strings));
            }
            serde_json::Value::Object(fields) => {
                fields.values().for_each(|value| collect(value, strings));
            }
            _ => {}
        }
    }
    let mut strings = Vec::new();
    collect(&serde_json::to_value(request).unwrap(), &mut strings);
    strings
}
