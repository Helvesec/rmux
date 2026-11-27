//! `#{alternate_saved_x}` and `#{alternate_saved_y}` publish the cursor the DEC
//! alternate screen saved on the *targeted* pane's own terminal screen.
//!
//! Every expectation comes from the primary text, never from the accessors under
//! test: tmux prints `wp->base.saved_cx` and `saved_cy` as `%u` and answers
//! nothing at all when there is no pane (`format.c:1369-1397`); `screen_init`
//! starts that pair at `UINT_MAX` (`screen.c:122-123`); `screen_alternate_on`
//! takes the cursor at `1049h` without moving it, and `screen_alternate_off`
//! restores the cursor at `1049l` and destroys the saved grid but leaves the
//! saved pair exactly as it is, so it survives the cycle (`screen.c:676-765`).
//!
//! The fixtures drive one live pane transcript through the whole cycle with the
//! existing sequential injection: the bytes reach the pane's own terminal the way
//! a reader publishes them, and no `Screen` is rebuilt at any state. That matters
//! here, because `Screen::new` leaves `preserve_alternate_screen_cursor` off
//! while the terminal parser turns it on, so a hand-built screen would not
//! represent a native `1049` entry at all.

use rmux_core::command_parser::CommandParser;
use rmux_core::formats::{FormatContext, FormatVariables};
use rmux_proto::{NewSessionExtRequest, PaneTarget, Request, SessionName, TerminalSize};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

use super::super::RequestHandler;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
/// `UINT_MAX`, the value tmux carries before anything has been saved.
const NEVER_SAVED: u32 = u32::MAX;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// A child that writes nothing on any platform, so the states a witness injects
/// are the only thing on the screen it reads back.
#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
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
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
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

async fn quiet_pane(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(SIZE),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&response);
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 0))
        .await;
    (handler, session)
}

/// Publishes one state's bytes into the pane's own live terminal, under the
/// transcript lock, and returns once they are consumed.
async fn feed(handler: &RequestHandler, session: &SessionName, pane: u32, bytes: &[u8]) {
    let mut state = handler.state.lock().await;
    state
        .append_bytes_to_pane_transcript_for_test(session, 0, pane, bytes)
        .expect("the pane consumes the state's bytes");
}

async fn grouped(handler: &RequestHandler, name: &str, pane: u32) -> String {
    queue(
        handler,
        &format!(
            "display-message -p -t {name}:0.{pane} 'on=#{{alternate_on}} sx=#{{alternate_saved_x}} sy=#{{alternate_saved_y}} cx=#{{cursor_x}} cy=#{{cursor_y}}'"
        ),
    )
    .await
}

async fn capture(handler: &RequestHandler, name: &str, pane: u32) -> String {
    queue(handler, &format!("capture-pane -p -t {name}:0.{pane}")).await
}

#[tokio::test]
async fn the_saved_cursor_follows_the_whole_alternate_cycle() {
    let (handler, session) = quiet_pane("asave-cycle").await;

    // S0, the normal screen: content, then the cursor parked at line 4 column 7,
    // which is 6,3 counted from zero. Nothing has been saved yet.
    feed(&handler, &session, 0, b"\x1b[1;1HNORMAL-A\x1b[4;7H").await;
    assert_eq!(
        grouped(&handler, "asave-cycle", 0).await,
        format!("on=0 sx={NEVER_SAVED} sy={NEVER_SAVED} cx=6 cy=3"),
        "before any alternate entry the saved pair is the maximum tmux starts from"
    );
    let normal = capture(&handler, "asave-cycle", 0).await;
    assert!(normal.contains("NORMAL-A"), "S0 shows the normal screen");

    // S1, entering the alternate screen: the cursor is taken as it stands and
    // does not move, and the alternate grid comes up blank.
    feed(&handler, &session, 0, b"\x1b[?1049h").await;
    assert_eq!(
        grouped(&handler, "asave-cycle", 0).await,
        "on=1 sx=6 sy=3 cx=6 cy=3",
        "1049h saves the cursor where it was and leaves it there"
    );
    let alternate = capture(&handler, "asave-cycle", 0).await;
    assert!(
        !alternate.contains("NORMAL-A"),
        "the alternate screen starts blank, without the normal content"
    );

    // S2, using the alternate screen: writing and moving inside it changes the
    // cursor and must leave the saved pair alone.
    feed(&handler, &session, 0, b"\x1b[3;3HALT-A\x1b[6;11H").await;
    assert_eq!(
        grouped(&handler, "asave-cycle", 0).await,
        "on=1 sx=6 sy=3 cx=10 cy=5",
        "working inside the alternate screen does not touch what was saved"
    );
    let used = capture(&handler, "asave-cycle", 0).await;
    assert!(used.contains("ALT-A"), "S2 shows what was written inside");
    assert!(!used.contains("NORMAL-A"));

    // S3, leaving it: the normal screen and its cursor come back, and the saved
    // pair is kept rather than reset.
    feed(&handler, &session, 0, b"\x1b[?1049l").await;
    assert_eq!(
        grouped(&handler, "asave-cycle", 0).await,
        "on=0 sx=6 sy=3 cx=6 cy=3",
        "1049l restores the cursor and keeps the saved pair"
    );
    let restored = capture(&handler, "asave-cycle", 0).await;
    assert_eq!(restored, normal, "the normal screen comes back identical");
    assert!(
        !restored.contains("ALT-A"),
        "and the alternate content is gone"
    );
}

#[tokio::test]
async fn a_screen_that_never_entered_the_alternate_answers_the_maximum() {
    let (handler, session) = quiet_pane("asave-plain").await;
    feed(&handler, &session, 0, b"\x1b[1;1HPLAIN\x1b[2;4H").await;
    assert_eq!(
        grouped(&handler, "asave-plain", 0).await,
        format!("on=0 sx={NEVER_SAVED} sy={NEVER_SAVED} cx=3 cy=1"),
        "a screen that never entered the alternate keeps the maximum"
    );
}

#[test]
fn no_pane_context_and_a_pane_without_a_screen_keep_their_absence() {
    // No pane at all.
    let empty = RuntimeFormatContext::new(FormatContext::new());
    for name in ["alternate_saved_x", "alternate_saved_y"] {
        assert_eq!(
            empty.format_value_by_name(name).as_deref(),
            Some(""),
            "with no pane context {name} keeps its existing empty result"
        );
    }

    // A pane that really exists, whose terminal screen does not: this fixture
    // holds a session, a window and a pane but no transcript.
    let mut state = HandlerState::default();
    let name = session_name("asave-noscreen");
    state
        .sessions
        .create_session(name.clone(), SIZE)
        .expect("the fixture creates its session");
    let session = state.sessions.session(&name).expect("the session exists");
    let window = session.window();
    let pane = session.active_pane().expect("the window has a pane");
    let runtime = RuntimeFormatContext::new(
        FormatContext::new()
            .with_managed_session(session)
            .with_window(0, window, true, false)
            .with_pane(pane, true),
    )
    .with_state(&state)
    .with_session(session)
    .with_window(0, window)
    .with_pane(pane);

    assert_eq!(
        render_runtime_template("session=#{session_name} id=#{pane_id}", &runtime, false),
        format!("session=asave-noscreen id={}", pane.id()),
        "the pane really is in this context; only its screen is missing"
    );
    assert_eq!(
        render_runtime_template(
            "x=[#{alternate_saved_x}] y=[#{alternate_saved_y}]",
            &runtime,
            false
        ),
        "x=[] y=[]",
        "a pane whose screen is absent keeps the existing empty result"
    );
}

#[tokio::test]
async fn an_explicitly_bound_value_wins_over_the_provider() {
    let (handler, session) = quiet_pane("asave-bound").await;
    feed(&handler, &session, 0, b"\x1b[4;7H\x1b[?1049h").await;

    let guard = handler.state.lock().await;
    let state = &*guard;
    let view = state
        .sessions
        .session(&session)
        .expect("the session exists");
    let window = view.window();
    let pane = view.active_pane().expect("the window has a pane");
    let runtime = RuntimeFormatContext::new(
        FormatContext::new()
            .with_managed_session(view)
            .with_window(0, window, true, false)
            .with_pane(pane, true),
    )
    .with_state(state)
    .with_session(view)
    .with_window(0, window)
    .with_pane(pane);

    // Same context twice: once binding nothing, once really binding the name.
    assert_eq!(
        render_runtime_template(
            "x=#{alternate_saved_x} y=#{alternate_saved_y}",
            &runtime,
            false
        ),
        "x=6 y=3",
        "with nothing bound the provider answers what the screen saved"
    );
    let bound = runtime
        .with_named_value("alternate_saved_x", "BX")
        .with_named_value("alternate_saved_y", "BY");
    assert_eq!(
        render_runtime_template(
            "x=#{alternate_saved_x} y=#{alternate_saved_y}",
            &bound,
            false
        ),
        "x=BX y=BY",
        "explicitly bound values are answered instead of the provider's reading"
    );
}

#[tokio::test]
async fn the_neighbour_pane_keeps_its_own_saved_cursor() {
    let (handler, session) = quiet_pane("asave-pair").await;
    queue(&handler, "split-window -v -l 7 -t asave-pair:0.0").await;

    // Only pane 0 goes through the cycle; pane 1 never enters the alternate.
    feed(
        &handler,
        &session,
        0,
        b"\x1b[1;1HNORMAL-A\x1b[4;7H\x1b[?1049h",
    )
    .await;

    // The window really holds the two panes this witness talks about.
    assert_eq!(
        queue(&handler, "list-panes -t asave-pair:0 -F '#{pane_index}'").await,
        "0\n1",
        "the fixture split its window in two"
    );
    // A comma inside a loop body is the separator between the ordinary body and
    // the one the active pane gets, so this body keeps its own.
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t asave-pair:0.0 '#{P:#{pane_index}=#{alternate_on}/#{alternate_saved_x}/#{alternate_saved_y} }'"
        )
        .await,
        format!("0=1/6/3 1=0/{NEVER_SAVED}/{NEVER_SAVED} "),
        "each pane answers for its own screen"
    );
    // The neighbour runs the default command, so its cursor is its own business;
    // what this witness bounds is the pair it answers for.
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t asave-pair:0.1 'on=#{alternate_on} sx=#{alternate_saved_x} sy=#{alternate_saved_y}'"
        )
        .await,
        format!("on=0 sx={NEVER_SAVED} sy={NEVER_SAVED}"),
        "the neighbour is untouched by its neighbour's alternate screen"
    );
}
