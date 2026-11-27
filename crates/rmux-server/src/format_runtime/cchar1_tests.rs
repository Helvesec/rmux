//! `#{cursor_character}` publishes the cell under the cursor of the *targeted*
//! pane's own terminal screen.
//!
//! Every expectation comes from the primary text, never from the accessor under
//! test: tmux reads `wp->base` at its own cursor and returns the cell's bytes
//! unless it is padding (`format.c:1195-1209`); the default cell is one space
//! (`grid.c:38-41`) and a column past a line's stored cells is that default cell
//! (`grid.c:581-589`); backspace decrements the column without erasing
//! (`screen-write.c:1103-1123`), so a letter followed by `BS` stays under the
//! cursor.
//!
//! The fixtures parse those bytes here and install the finished screen with the
//! transcript's own `set_screen_for_test`, so consumption is established by
//! construction: nothing has to recognise a positioning after the fact, and the
//! pane's child is a quiet command on every platform, so it cannot race what the
//! fixture installed.
//!
//! Scope of each witness, stated plainly: the reading witnesses (backspace,
//! blank, UTF-8) are the ones a provider that answers a constant falsifies. The
//! absence, copy-mode and binding witnesses are guards on properties that
//! constant has no bearing on; they are not presented as proof that those
//! properties were ever broken.

use rmux_core::command_parser::CommandParser;
use rmux_core::formats::{FormatContext, FormatVariables};
use rmux_core::{input::InputParser, Screen};
use rmux_proto::{NewSessionExtRequest, PaneTarget, Request, SessionName, TerminalSize};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

use super::super::RequestHandler;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// A child that writes nothing on any platform, so the screen a witness installs
/// is the screen the witness reads back.
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

/// A detached 80x24 session whose only pane runs the quiet child.
async fn quiet_pane(name: &str) -> (RequestHandler, SessionName, PaneTarget) {
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
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    (handler, session, target)
}

/// A pane with no process at all.
///
/// `split-window -I` with no command makes the empty pane the product already
/// supports (`handler_scripting/pane_parse.rs:499`, `pane_lifecycle/split.rs:172`):
/// it owns a transcript to read and no console behind it. That matters for this
/// witness alone, because entering copy mode resizes the session's terminals
/// (`handler_copy_mode.rs:204-212` through `pane_lifecycle.rs:428-445` and
/// `native_resize.rs:156-176`, which resizes even when the geometry has not
/// changed), and a console answers a resize by re-emitting its own buffer - which
/// holds what the child wrote, never what a fixture installed. `native_resize.rs`
/// skips a pane with no process, so nothing can repaint this one on any platform.
async fn empty_pane(name: &str) -> (RequestHandler, PaneTarget) {
    let (handler, session, _target) = quiet_pane(name).await;
    queue(&handler, &format!("split-window -I -t {name}:0.0")).await;
    (handler, PaneTarget::with_window(session, 0, 1))
}

/// The pane's own terminal screen, the one this value reads. `capture-pane`
/// takes the mode screen only with `-M`, so this stays the terminal screen even
/// while the pane is in copy mode.
async fn capture_at(handler: &RequestHandler, target: &str) -> String {
    queue(handler, &format!("capture-pane -p -t {target}")).await
}

async fn cursor_character_at(handler: &RequestHandler, target: &str) -> String {
    queue(
        handler,
        &format!("display-message -p -t {target} 'c=[#{{cursor_character}}]'"),
    )
    .await
}

/// The size this pane's own screen already has, so an installed screen keeps it.
async fn pane_screen_size(handler: &RequestHandler, target: &PaneTarget) -> TerminalSize {
    let state = handler.state.lock().await;
    let transcript = state
        .transcript_handle(target)
        .expect("the pane has a transcript");
    let size = transcript.lock().screen().size();
    size
}

/// Parses `bytes` into a screen built with the pane's own history limit and UTF-8
/// configuration, then installs that finished screen. When this returns, the
/// cursor already sits where the sequence left it; the position it reports comes
/// from the parse, not from any format.
async fn install_screen(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) -> (u32, u32) {
    install_screen_sized(handler, target, SIZE, bytes).await
}

/// The same installation for a pane whose own geometry is not the window's.
async fn install_screen_sized(
    handler: &RequestHandler,
    target: &PaneTarget,
    size: TerminalSize,
    bytes: &[u8],
) -> (u32, u32) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("the pane has a transcript")
    };
    let (history_limit, utf8_config) = {
        let transcript = transcript.lock();
        (transcript.history_limit(), transcript.utf8_config().clone())
    };
    let mut screen = Screen::new(size, history_limit);
    screen.set_utf8_config(utf8_config);
    InputParser::new().parse(bytes, &mut screen);
    let position = screen.cursor_position();
    transcript.lock().set_screen_for_test(screen);
    position
}

async fn cursor_character(handler: &RequestHandler, name: &str) -> String {
    queue(
        handler,
        &format!("display-message -p -t {name}:0.0 'c=[#{{cursor_character}}]'"),
    )
    .await
}

#[tokio::test]
async fn cursor_character_reads_the_cell_left_under_the_cursor_by_backspace() {
    let (handler, _session, target) = quiet_pane("cchar-bs").await;
    // Column 5, the letter, then BS: the cursor comes back onto the letter it
    // just wrote, which is the primary counterexample.
    let position = install_screen(&handler, &target, b"\x1b[1;5HA\x08").await;
    assert_eq!(
        position,
        (4, 0),
        "the parse leaves the cursor back on the letter it wrote"
    );
    assert_eq!(
        cursor_character(&handler, "cchar-bs").await,
        "c=[A]",
        "the cell the cursor sits on still holds the letter that was written"
    );
}

#[tokio::test]
async fn a_real_blank_cell_reads_as_one_space() {
    let (handler, _session, target) = quiet_pane("cchar-blank").await;
    // Nothing was ever written on this row, so the column the cursor lands on is
    // a real blank cell, and a column past what the line stores is the same
    // default cell.
    let position = install_screen(&handler, &target, b"\x1b[1;40H").await;
    assert_eq!(position, (39, 0));
    assert_eq!(
        cursor_character(&handler, "cchar-blank").await,
        "c=[ ]",
        "an untouched cell is the default cell, one space"
    );
}

#[tokio::test]
async fn a_full_utf8_cell_is_returned_whole_and_its_padding_is_absent() {
    let (handler, _session, target) = quiet_pane("cchar-utf8").await;
    // The character is written and the cursor is then put back on its own cell
    // by an explicit positioning.
    let position = install_screen(&handler, &target, "\x1b[1;5Hé\x1b[1;5H".as_bytes()).await;
    assert_eq!(position, (4, 0));
    assert_eq!(
        cursor_character(&handler, "cchar-utf8").await,
        "c=[é]",
        "the cell's whole text is returned, not its first byte"
    );

    // A wide character owns two columns: the head carries the text, and its
    // padding half carries none at all.
    let position = install_screen(&handler, &target, "\x1b[1;5H漢\x1b[1;5H".as_bytes()).await;
    assert_eq!(position, (4, 0));
    assert_eq!(
        cursor_character(&handler, "cchar-utf8").await,
        "c=[漢]",
        "the head of a wide character returns the whole character"
    );

    let position = install_screen(&handler, &target, "\x1b[1;5H漢\x1b[1;6H".as_bytes()).await;
    assert_eq!(position, (5, 0));
    assert_eq!(
        cursor_character(&handler, "cchar-utf8").await,
        "c=[]",
        "its padding half has no text of its own"
    );
}

#[test]
fn no_pane_context_keeps_its_absence() {
    let context = RuntimeFormatContext::new(FormatContext::new());
    assert_eq!(
        context.format_value_by_name("cursor_character").as_deref(),
        Some(""),
        "with no pane context at all the registered name keeps its existing empty result"
    );
    assert_eq!(
        render_runtime_template("c=[#{cursor_character}]", &context, false),
        "c=[]"
    );
}

#[test]
fn an_existing_pane_without_a_screen_keeps_its_absence() {
    // Distinct from the case above: here the session, the window and the pane all
    // exist, and only the pane's terminal screen is absent, because this fixture
    // holds no transcript.
    let mut state = HandlerState::default();
    let name = session_name("cchar-noscreen");
    state
        .sessions
        .create_session(name.clone(), SIZE)
        .expect("the fixture creates its session");
    let session = state.sessions.session(&name).expect("the session exists");
    let window = session.window();
    let pane = session.active_pane().expect("the window has a pane");
    let base = FormatContext::new()
        .with_managed_session(session)
        .with_window(0, window, true, false)
        .with_pane(pane, true);
    let runtime = RuntimeFormatContext::new(base)
        .with_state(&state)
        .with_session(session)
        .with_window(0, window)
        .with_pane(pane);

    assert_eq!(
        render_runtime_template(
            "session=#{session_name} pane=#{pane_index} id=#{pane_id}",
            &runtime,
            false
        ),
        format!("session=cchar-noscreen pane=0 id={}", pane.id()),
        "the pane really is in this context; only its screen is missing"
    );
    assert_eq!(
        runtime.format_value_by_name("cursor_character").as_deref(),
        Some(""),
        "a pane whose screen is absent keeps the existing empty result"
    );
    assert_eq!(
        render_runtime_template("c=[#{cursor_character}]", &runtime, false),
        "c=[]"
    );
}

#[tokio::test]
async fn the_terminal_screen_is_read_not_the_copy_mode_render() {
    // This witness is the only one that keeps reading after copy mode has been
    // entered, and entering copy mode resizes the session's terminals. A console
    // answers that resize by re-emitting its own buffer, which is what the child
    // wrote and never what a fixture installed. So this witness alone runs on a
    // pane that has no process to repaint it; the five others keep their fixture.
    let (handler, target) = empty_pane("cchar-copy").await;
    let size = pane_screen_size(&handler, &target).await;
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t cchar-copy:0.1 'pid=[#{pane_pid}] dead=[#{pane_dead}] size=#{pane_width}x#{pane_height}'"
        )
        .await,
        "pid=[0] dead=[0] size=80x11",
        "the fixture's pane really has no process behind it, and is not a dead one either"
    );
    // Row 1 holds A then Z from column 5; row 2 holds six Qs from column 1. The
    // terminal cursor is left on the A.
    let position = install_screen_sized(
        &handler,
        &target,
        size,
        b"\x1b[1;5HAZ\x1b[2;1HQQQQQQ\x1b[1;5H",
    )
    .await;
    assert_eq!(position, (4, 0));
    assert_eq!(
        cursor_character_at(&handler, "cchar-copy:0.1").await,
        "c=[A]"
    );

    queue(&handler, "copy-mode -t cchar-copy:0.1").await;
    queue(&handler, "send-keys -t cchar-copy:0.1 -X cursor-right").await;
    queue(&handler, "send-keys -t cchar-copy:0.1 -X cursor-down").await;

    // The copy render now has a cursor of its own, on another row and another
    // column: column 6 of row 2, where the fixture wrote a Q, not the A. The
    // pre-existing cursor_x/cursor_y follow that render, which is exactly the
    // reading this value must not take.
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t cchar-copy:0.1 'mode=#{pane_in_mode} copy=#{copy_cursor_x},#{copy_cursor_y} render=#{cursor_x},#{cursor_y}'"
        )
        .await,
        "mode=1 copy=5,1 render=5,1",
        "the copy render's cursor really moved to a different row and column"
    );
    // Read the terminal screen itself just before the value does. If the pane's
    // own output had landed late and replaced the fixture, the cell under the
    // cursor would be blank for a reason that has nothing to do with the value
    // under test, and that case is named here instead of being blamed on the
    // provider.
    let live = capture_at(&handler, "cchar-copy:0.1").await;
    assert!(
        live.contains("AZ") && live.contains("QQQQQQ"),
        "the fixture's own screen is still the pane's terminal screen; capture was {live:?}"
    );

    assert_eq!(
        cursor_character_at(&handler, "cchar-copy:0.1").await,
        "c=[A]",
        "the copy-mode cursor moved onto another cell; the terminal cell under the terminal cursor did not"
    );
}

#[tokio::test]
async fn an_explicitly_bound_value_wins_over_the_provider() {
    let (handler, name, target) = quiet_pane("cchar-bound").await;
    let position = install_screen(&handler, &target, b"\x1b[1;5HA\x08").await;
    assert_eq!(position, (4, 0));

    let guard = handler.state.lock().await;
    let state = &*guard;
    let session = state.sessions.session(&name).expect("the session exists");
    let window = session.window();
    let pane = session.active_pane().expect("the window has a pane");
    let base = FormatContext::new()
        .with_managed_session(session)
        .with_window(0, window, true, false)
        .with_pane(pane, true);
    let runtime = RuntimeFormatContext::new(base)
        .with_state(state)
        .with_session(session)
        .with_window(0, window)
        .with_pane(pane);

    // Same context twice: once as a caller that binds nothing, once as a caller
    // that really binds this name. The lookup consults the bound value before the
    // provider, and that order is unchanged.
    assert_eq!(
        render_runtime_template("c=[#{cursor_character}]", &runtime, false),
        "c=[A]",
        "with nothing bound the provider answers the cell under the cursor"
    );
    let bound = runtime.with_named_value("cursor_character", "BOUND");
    assert_eq!(
        render_runtime_template("c=[#{cursor_character}]", &bound, false),
        "c=[BOUND]",
        "an explicitly bound value is answered instead of the provider's reading"
    );
}
