//! `#{window_active_sessions}` and `#{window_active_sessions_list}` describe the
//! window, from any of its links.
//!
//! Every expectation below is a literal measured on the pinned tmux 3.7b before
//! this file existed, with these very commands: two detached sessions sharing one
//! window read `1|zeta`, then `2|zeta,alpha`, then `1|alpha` from the link that is
//! not selected, then `0|`; a second alias of the window in zeta, current while
//! the first stays inactive, reads `2|alpha,zeta` — the order links were added,
//! not an alphabetical one — and counts zeta once. A context with no window link
//! answers nothing for either name.

use rmux_core::command_parser::CommandParser;
use rmux_proto::{NewSessionExtRequest, Request, SessionName, TerminalSize};

use crate::handler::RequestHandler;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const FMT: &str = "'#{window_active_sessions}|#{window_active_sessions_list}'";

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

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

/// Puts each session on the window this step is about, then states where both
/// sessions stand before anything is read.
async fn select(handler: &RequestHandler, zeta: &str, alpha: &str, expected: &str) {
    queue(handler, &format!("select-window -t {zeta}")).await;
    queue(handler, &format!("select-window -t {alpha}")).await;
    let seen = queue(
        handler,
        "list-windows -a -F '#{session_name}:#{window_index}=#{window_active}'",
    )
    .await;
    let seen = seen.split('\n').collect::<Vec<_>>().join(" ");
    let mut wanted = expected.split(' ').collect::<Vec<_>>();
    wanted.sort_unstable();
    let mut measured = seen.split(' ').collect::<Vec<_>>();
    measured.sort_unstable();
    assert_eq!(
        measured, wanted,
        "the fixture is where this step says it is"
    );
}

async fn read(handler: &RequestHandler, target: &str) -> String {
    queue(handler, &format!("display-message -p -t {target} {FMT}")).await
}

#[tokio::test]
async fn both_links_answer_for_the_window_through_every_selection() {
    let handler = shared_window().await;

    // Each step states the selection it depends on before reading, so a surprise
    // in the fixture is named instead of being read as an answer about W.
    select(
        &handler,
        "zeta:1",
        "alpha:0",
        "zeta:0=0 zeta:1=1 alpha:0=1 alpha:1=0",
    )
    .await;
    assert_eq!(read(&handler, "zeta:1").await, "1|zeta");
    assert_eq!(
        read(&handler, "alpha:1").await,
        "1|zeta",
        "the link that is not selected answers for the window all the same"
    );

    select(
        &handler,
        "zeta:1",
        "alpha:1",
        "zeta:0=0 zeta:1=1 alpha:0=0 alpha:1=1",
    )
    .await;
    assert_eq!(read(&handler, "zeta:1").await, "2|zeta,alpha");
    assert_eq!(read(&handler, "alpha:1").await, "2|zeta,alpha");

    select(
        &handler,
        "zeta:0",
        "alpha:1",
        "zeta:0=1 zeta:1=0 alpha:0=0 alpha:1=1",
    )
    .await;
    assert_eq!(
        read(&handler, "zeta:1").await,
        "1|alpha",
        "a link can name the other session while its own is elsewhere"
    );

    select(
        &handler,
        "zeta:0",
        "alpha:0",
        "zeta:0=1 zeta:1=0 alpha:0=1 alpha:1=0",
    )
    .await;
    assert_eq!(
        read(&handler, "zeta:1").await,
        "0|",
        "the count stays 0 and the list answers nothing"
    );
}

#[tokio::test]
async fn a_second_alias_counts_its_session_once_and_keeps_link_order() {
    let handler = shared_window().await;
    queue(&handler, "link-window -d -s zeta:1 -t zeta:2").await;
    queue(&handler, "select-window -t zeta:2").await;
    queue(&handler, "select-window -t alpha:1").await;

    // zeta is current on its second alias while its first is not the window it is
    // on: it is counted once, and the order is the order the links were added.
    assert_eq!(read(&handler, "zeta:1").await, "2|alpha,zeta");
    assert_eq!(read(&handler, "zeta:2").await, "2|alpha,zeta");
    assert_eq!(read(&handler, "alpha:1").await, "2|alpha,zeta");
}

#[tokio::test]
async fn list_windows_renders_and_filters_on_the_same_values() {
    let handler = shared_window().await;
    queue(&handler, "link-window -d -s zeta:1 -t zeta:2").await;
    queue(&handler, "select-window -t zeta:2").await;
    queue(&handler, "select-window -t alpha:1").await;

    assert_eq!(
        queue(
            &handler,
            "list-windows -t zeta -F '#{window_index}:#{window_active_sessions}:#{window_active_sessions_list}'"
        )
        .await,
        "0:0:\n1:2:alpha,zeta\n2:2:alpha,zeta"
    );
    assert_eq!(
        queue(
            &handler,
            "list-windows -t zeta -F '#{window_index}' -f '#{==:#{window_active_sessions},2}'"
        )
        .await,
        "1\n2",
        "the filter reads the same values the template renders"
    );
}

#[tokio::test]
async fn a_context_without_a_window_link_answers_nothing() {
    let handler = shared_window().await;
    queue(&handler, "set-buffer -b probe X").await;

    assert_eq!(
        queue(
            &handler,
            "list-buffers -F 'count=[#{window_active_sessions}] list=[#{window_active_sessions_list}]'"
        )
        .await,
        "count=[] list=[]",
        "a buffer line carries no window, so neither name has a value"
    );
}

/// tmux 3.7b, measured for this regression: joining alpha to zulu keeps
/// zulu's original winlink first, whichever member supplies the format context.
async fn simply_grouped_window() -> RequestHandler {
    let handler = RequestHandler::new();
    new_session(&handler, "zulu", "W").await;
    queue(&handler, "new-session -d -s alpha -t zulu").await;
    queue(&handler, "select-window -t zulu:0").await;
    queue(&handler, "select-window -t alpha:0").await;
    handler
}

#[tokio::test]
async fn grouped_active_sessions_keep_winlink_order_from_both_members() {
    let handler = simply_grouped_window().await;
    assert_eq!(read(&handler, "zulu:0").await, "2|zulu,alpha");
    assert_eq!(read(&handler, "alpha:0").await, "2|zulu,alpha");
}

#[tokio::test]
async fn grouped_active_sessions_render_and_filter_in_winlink_order() {
    let handler = simply_grouped_window().await;
    assert_eq!(
        queue(
            &handler,
            "list-windows -a -F '#{session_name}:#{window_active_sessions}:#{window_active_sessions_list}'"
        )
        .await,
        "alpha:2:zulu,alpha\nzulu:2:zulu,alpha"
    );
    assert_eq!(
        queue(
            &handler,
            "list-windows -t alpha -F '#{window_index}' -f '#{m:zulu*,#{window_active_sessions_list}}'"
        )
        .await,
        "0"
    );
}

#[tokio::test]
async fn grouped_active_sessions_keep_interleaved_links_across_two_groups() {
    let handler = RequestHandler::new();
    new_session(&handler, "zulu", "W").await;
    new_session(&handler, "beta", "fallback").await;
    queue(&handler, "link-window -d -s zulu:0 -t beta:1").await;
    queue(&handler, "new-session -d -s alpha -t zulu").await;
    queue(&handler, "new-session -d -s delta -t beta").await;
    for target in ["zulu:0", "alpha:0", "beta:1", "delta:1"] {
        queue(&handler, &format!("select-window -t {target}")).await;
    }

    // Measured on tmux 3.7b: the outsider's link precedes both later JOINs.
    // Group membership order cannot reconstruct this interleaving.
    for target in ["zulu:0", "alpha:0", "beta:1", "delta:1"] {
        assert_eq!(read(&handler, target).await, "4|zulu,beta,alpha,delta");
    }
    queue(&handler, "select-window -t beta:0").await;
    for target in ["zulu:0", "alpha:0", "beta:1", "delta:1"] {
        assert_eq!(read(&handler, target).await, "3|zulu,alpha,delta");
        assert_eq!(
            queue(
                &handler,
                &format!("display-message -p -t {target} '#{{window_linked_sessions_list}}'")
            )
            .await,
            "zulu,beta,alpha,delta",
            "the linked list still includes the inactive member in winlink order"
        );
    }
}
