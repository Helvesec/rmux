//! The sessions where a linked window is the current one.
//!
//! tmux walks the window's links in the order they were added and keeps each
//! link whose session has that link as its current window (`format.c:718-767`,
//! `window.c:185-192`). A session that owns two links therefore counts once,
//! through whichever of them it is currently on — including when the first link
//! it owns is not that one. Those orders and values were measured on the pinned
//! tmux 3.7b before this file existed.
//!
//! The fixture goes through the real commands because a window link needs real
//! pane terminals: `link_window` fails with "missing pane terminals" on a state
//! that has none, which is why every link test in this crate drives the handler.

use rmux_proto::{NewSessionExtRequest, Request, SessionName, TerminalSize};

use rmux_core::command_parser::CommandParser;

use crate::handler::RequestHandler;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// A child that writes nothing on any platform.
#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "sleep 300".to_owned(),
    ]
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 600 127.0.0.1 >NUL".to_owned(),
    ]
}

/// The same child as a single command line, for the commands that take one.
/// Joining the argv with spaces would not do: the parser would split `sleep 300`
/// into two arguments and the window would die at once, taking its links with it.
#[cfg(unix)]
fn quiet_command_line() -> String {
    "/bin/sleep 300".to_owned()
}

#[cfg(windows)]
fn quiet_command_line() -> String {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let ping = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("ping.exe");
    format!("{} -n 600 127.0.0.1", ping.to_string_lossy())
}

async fn queue(handler: &RequestHandler, command: &str) -> String {
    let output = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new()
                .parse(command)
                .expect("the command parses"),
        )
        .await
        .expect("the command succeeds");
    String::from_utf8(output.stdout().to_vec())
        .expect("stdout is utf-8")
        .trim_end_matches('\n')
        .to_owned()
}

async fn new_session(handler: &RequestHandler, name: &str, window_name: &str) {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name(name)),
            working_directory: None,
            detached: true,
            size: Some(SIZE),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: Some(window_name.to_owned()),
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);
}

/// zeta holds a fallback window and the shared window W at index 1; alpha holds
/// a fallback window and a link to W at its own index 1.
async fn shared_window() -> RequestHandler {
    let handler = RequestHandler::new();
    new_session(&handler, "zeta", "fallbackZ").await;
    queue(
        &handler,
        &format!("new-window -d -t zeta: -n W {}", quiet_command_line()),
    )
    .await;
    new_session(&handler, "alpha", "fallbackA").await;
    queue(&handler, "link-window -d -s zeta:1 -t alpha:1").await;
    handler
}

/// The aggregator itself, read through the state the handler owns.
async fn current(handler: &RequestHandler, session: &str, window_index: u32) -> Vec<String> {
    let state = handler.state.lock().await;
    state
        .window_linked_current_sessions_list(&session_name(session), window_index)
        .into_iter()
        .map(|name| name.to_string())
        .collect()
}

#[tokio::test]
async fn both_sessions_are_listed_when_the_shared_window_is_current_in_each() {
    let handler = shared_window().await;
    queue(&handler, "select-window -t zeta:1").await;
    queue(&handler, "select-window -t alpha:1").await;

    // Read from either link: the answer describes the window, not the link.
    assert_eq!(current(&handler, "zeta", 1).await, ["zeta", "alpha"]);
    assert_eq!(current(&handler, "alpha", 1).await, ["zeta", "alpha"]);
}

#[tokio::test]
async fn a_session_that_left_the_shared_window_stops_being_listed() {
    let handler = shared_window().await;
    queue(&handler, "select-window -t zeta:0").await;
    queue(&handler, "select-window -t alpha:1").await;
    assert_eq!(current(&handler, "zeta", 1).await, ["alpha"]);

    queue(&handler, "select-window -t alpha:0").await;
    assert!(current(&handler, "zeta", 1).await.is_empty());
}

#[tokio::test]
async fn a_second_alias_is_not_masked_by_the_first() {
    let handler = shared_window().await;
    // zeta takes a second link to the same window and is current on that one,
    // while the first link it owns is not the window it is on.
    queue(&handler, "link-window -d -s zeta:1 -t zeta:2").await;
    queue(&handler, "select-window -t zeta:2").await;
    queue(&handler, "select-window -t alpha:1").await;

    // The links are walked in the order they were added, so the active ones read
    // alpha then zeta, and zeta is there exactly once.
    assert_eq!(current(&handler, "zeta", 1).await, ["alpha", "zeta"]);
    assert_eq!(current(&handler, "zeta", 2).await, ["alpha", "zeta"]);
    assert_eq!(current(&handler, "alpha", 1).await, ["alpha", "zeta"]);
}

#[tokio::test]
async fn a_session_holding_two_aliases_is_listed_once() {
    let handler = shared_window().await;
    queue(&handler, "link-window -d -s zeta:1 -t zeta:2").await;
    queue(&handler, "select-window -t zeta:1").await;
    queue(&handler, "select-window -t alpha:0").await;

    assert_eq!(current(&handler, "zeta", 1).await, ["zeta"]);
}
