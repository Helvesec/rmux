//! `#{session_stack}` and `#{window_stack_index}` describe the windows the
//! session really visited.
//!
//! Every literal below was measured on the pinned tmux 3.7b with these very
//! commands, before this file existed: four windows with 0 active and 1, 2 and 3
//! created detached read `0`, then `3,0`, `1,3,0`, `2,1,3,0` and, after
//! last-window, `1,2,3,0`. In the listing a window never visited ranks 0, the
//! current one ranks 0, and the others rank by real recency. Killing an older
//! occurrence leaves `1,2,0` and no rank for anything dead.

use rmux_core::command_parser::CommandParser;
use rmux_proto::{NewSessionExtRequest, Request, SessionName, TerminalSize};

use crate::handler::RequestHandler;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const LIST: &str = "list-windows -t stack -F '#{window_index}|#{window_id}|#{window_stack_index}'";

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    vec!["/bin/sleep".to_owned(), "300".to_owned()]
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let ping = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("ping.exe");
    vec![
        ping.to_string_lossy().into_owned(),
        "-n".to_owned(),
        "600".to_owned(),
        "127.0.0.1".to_owned(),
    ]
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

/// A detached session with window 0 active and windows 1, 2 and 3 created
/// detached, so none of the three has ever been visited.
async fn four_windows() -> RequestHandler {
    let handler = RequestHandler::new();
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name("stack")),
            working_directory: None,
            detached: true,
            size: Some(SIZE),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: Some("w0".to_owned()),
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);
    for name in ["w1", "w2", "w3"] {
        queue(
            &handler,
            &format!(
                "new-window -d -t stack: -n {name} {}",
                quiet_command().join(" ")
            ),
        )
        .await;
    }
    handler
}

async fn stack(handler: &RequestHandler) -> String {
    queue(handler, "display-message -p -t stack '#{session_stack}'").await
}

#[tokio::test]
async fn a_window_never_visited_is_in_no_stack_and_ranks_zero() {
    let handler = four_windows().await;

    assert_eq!(stack(&handler).await, "0");
    assert_eq!(
        queue(&handler, LIST).await,
        "0|@0|0\n1|@1|0\n2|@2|0\n3|@3|0",
        "three windows exist that were never visited, and none of them has a rank"
    );
}

#[tokio::test]
async fn the_stack_follows_the_real_visits() {
    let handler = four_windows().await;

    queue(&handler, "select-window -t stack:3").await;
    assert_eq!(stack(&handler).await, "3,0");
    queue(&handler, "select-window -t stack:1").await;
    assert_eq!(stack(&handler).await, "1,3,0");
    queue(&handler, "select-window -t stack:2").await;
    assert_eq!(stack(&handler).await, "2,1,3,0");

    assert_eq!(
        queue(&handler, LIST).await,
        "0|@0|3\n1|@1|1\n2|@2|0\n3|@3|2",
        "the current window ranks 0 and the others by real recency"
    );
}

#[tokio::test]
async fn the_return_goes_to_the_head_and_pushes_what_it_leaves() {
    let handler = four_windows().await;
    for index in [3, 1, 2] {
        queue(&handler, &format!("select-window -t stack:{index}")).await;
    }

    queue(&handler, "last-window -t stack").await;

    assert_eq!(stack(&handler).await, "1,2,3,0");
    assert_eq!(
        queue(&handler, LIST).await,
        "0|@0|3\n1|@1|0\n2|@2|1\n3|@3|2"
    );
}

#[tokio::test]
async fn a_killed_window_is_not_recalled_by_the_stack_or_by_the_return() {
    let handler = four_windows().await;
    for index in [3, 1, 2] {
        queue(&handler, &format!("select-window -t stack:{index}")).await;
    }
    queue(&handler, "last-window -t stack").await;

    queue(&handler, "kill-window -t stack:3").await;
    assert_eq!(stack(&handler).await, "1,2,0");
    assert_eq!(queue(&handler, LIST).await, "0|@0|2\n1|@1|0\n2|@2|1");

    // Going back again must land on a window that still exists, and the stack
    // must still name only live occurrences.
    queue(&handler, "last-window -t stack").await;
    assert_eq!(stack(&handler).await, "2,1,0");
    assert_eq!(queue(&handler, LIST).await, "0|@0|2\n1|@1|1\n2|@2|0");
}
