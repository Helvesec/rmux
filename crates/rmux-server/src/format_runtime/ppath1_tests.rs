//! `#{pane_path}` publishes the path the *targeted* pane's own shell announced.
//!
//! The expectations come from the primary text, never from the accessor under
//! test: tmux answers `wp->base.path`, the empty string when that path was never
//! set, and no value at all when the format has no pane (`format.c:2268-2277`).
//! What sets it is OSC 7 on the pane's own screen (`input.c:2713-2717` calling
//! `screen_set_path`, `screen.c:261-272`), and for a printable URI the whole
//! announcement is kept as it arrived (`tmux.c:285-299`): the host and the `%20`
//! stay, nothing is decoded and no working directory is invented. Measured once
//! on the pinned tmux 3.7b before these witnesses were written
//! (`ppath1/native/oracle-attempt2.txt`).
//!
//! Each witness is causal end to end: the announcement is the real OSC 7 byte
//! sequence, consumed by the pane's own parser into its own `Screen`, and the
//! reading is the runtime resolution every caller uses. Nothing here mirrors the
//! accessor, and no witness reads a pane whose child could write on its own.
//!
//! Announced and replacement witnesses require the pane's own raw value. The
//! absence and retired-terminal checks also carry a same-name environment
//! sentinel: an unavailable provider must stop resolution before that tier.
//! The silent neighbor remains a real empty value, so its path modifiers still
//! return `.`. These modifier expectations were measured on tmux 3.7b alongside
//! the buffer format/filter reproduction.

use std::path::Path;

use rmux_core::formats::{FormatContext, FormatVariables};
use rmux_core::{ManagedSessionView, PaneId};
use rmux_proto::{
    PaneTarget, ProcessCommand, SessionName, SplitDirection, SplitWindowTarget, TerminalSize,
};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::{HandlerState, InitialPaneSpawnOptions, SplitPaneProcess};

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const SOCKET: &str = "/tmp/rmux-ppath1-test.sock";
const FIRST: &str = "file://ppath-host/tmp/ppath%20one";
const SECOND: &str = "file://ppath-host/var/ppath%20two";
const ELSEWHERE: &str = "/ppath/elsewhere";

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// A child that writes nothing of its own on any platform, so no pane of this
/// fixture can announce a path the witnesses did not put there.
#[cfg(unix)]
fn quiet_command() -> ProcessCommand {
    ProcessCommand::Argv(vec!["/bin/cat".to_owned()])
}

#[cfg(windows)]
fn quiet_command() -> ProcessCommand {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    ProcessCommand::Argv(vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ])
}

/// A session whose two witness panes have no process at all.
///
/// The session's first pane holds the quiet child, because the pane terminal
/// store has to exist before a pane can be split into it, and no witness reads
/// that pane. The two panes the witnesses do read are empty ones: they own a
/// transcript to parse into and have no console behind them, so what they
/// answer is what these witnesses put there and never what a shell announced
/// for itself.
fn fixture(name: &str) -> (HandlerState, SessionName, PaneId, PaneId) {
    let mut state = HandlerState::default();
    let session = session_name(name);
    state
        .sessions
        .create_session(session.clone(), SIZE)
        .expect("the session is created");
    // A path that exists in this context and is not this pane's: the witnesses
    // that read an empty announcement have something to be confused with.
    state.environment.set(
        rmux_proto::ScopeSelector::Global,
        "PWD".to_owned(),
        ELSEWHERE.to_owned(),
    );
    state.environment.set(
        rmux_proto::ScopeSelector::Global,
        "pane_path".to_owned(),
        "spoof".to_owned(),
    );
    let quiet = quiet_command();
    state
        .insert_initial_session_terminal(
            &session,
            InitialPaneSpawnOptions {
                socket_path: Path::new(SOCKET),
                spawn_environment: None,
                raw_spawn_environment: None,
                environment_overrides: None,
                command: Some(&quiet),
                pane_alert_callback: None,
                pane_exit_callback: None,
            },
        )
        .expect("the session's first pane exists");
    // Each split is taken by the identity it created, never by the index it had
    // at the time: a later split of the same window renumbers the panes after
    // it, and two witnesses must not end up reading one pane.
    let first = empty_pane(&mut state, &session);
    let second = empty_pane(&mut state, &session);
    (state, session, first, second)
}

/// One more pane of that window, with no process at all, taken by the identity
/// the split itself reports.
fn empty_pane(state: &mut HandlerState, session: &SessionName) -> PaneId {
    let target = state
        .split_window(
            SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
            SplitDirection::Vertical,
            false,
            Path::new(SOCKET),
            None,
            None,
            SplitPaneProcess::Empty,
            None,
            None,
            None,
            false,
            true,
            None,
            None,
            None,
        )
        .expect("an empty witness pane is created")
        .pane;
    state
        .sessions
        .session(session)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.pane(target.pane_index()))
        .map(rmux_core::Pane::id)
        .expect("the pane the split reported is in the window")
}

/// Where that pane sits in its window now, which a later split can change.
fn pane_index(state: &HandlerState, session: &SessionName, pane: PaneId) -> u32 {
    state
        .sessions
        .session(session)
        .and_then(|session| session.window_at(0))
        .and_then(|window| {
            window
                .panes()
                .iter()
                .find(|candidate| candidate.id() == pane)
                .map(rmux_core::Pane::index)
        })
        .expect("the fixture pane is in the window")
}

/// The announcement itself: the OSC 7 bytes a shell writes, given to the pane's
/// own parser through the transcript that pane owns.
fn announce(state: &mut HandlerState, session: &SessionName, pane: PaneId, uri: &str) {
    let index = pane_index(state, session, pane);
    let mut bytes = b"\x1b]7;".to_vec();
    bytes.extend_from_slice(uri.as_bytes());
    bytes.extend_from_slice(b"\x1b\\");
    state
        .append_bytes_to_pane_transcript_for_test(session, 0, index, &bytes)
        .expect("the pane consumes its own announcement");
}

/// What the runtime answers for a variable with this pane targeted.
fn context_at<'a>(
    state: &'a HandlerState,
    session: &SessionName,
    pane: PaneId,
) -> RuntimeFormatContext<'a> {
    let view: ManagedSessionView<'_> = state.sessions.session(session).expect("the session");
    let window = view.window_at(0).expect("the window");
    let pane = window
        .panes()
        .iter()
        .find(|candidate| candidate.id() == pane)
        .expect("the pane");
    RuntimeFormatContext::new(FormatContext::new())
        .with_state(state)
        .with_session(view)
        .with_window(0, window)
        .with_pane(pane)
}

fn value_at(
    state: &HandlerState,
    session: &SessionName,
    pane: PaneId,
    variable: &str,
) -> Option<String> {
    context_at(state, session, pane).format_value_by_name(variable)
}

fn modified_path_at(state: &HandlerState, session: &SessionName, pane: PaneId) -> String {
    render_runtime_template(
        "[#{pane_path}]|[#{b:pane_path}]|[#{d:pane_path}]",
        &context_at(state, session, pane),
        false,
    )
}

fn path_at(state: &HandlerState, session: &SessionName, pane: PaneId) -> Option<String> {
    value_at(state, session, pane, "pane_path")
}

#[tokio::test]
async fn the_pane_that_announced_a_path_answers_it_and_its_neighbour_does_not() {
    let (mut state, session, first, second) = fixture("ppath-announced");
    announce(&mut state, &session, first, FIRST);

    // The whole URI, exactly as it was announced: the host is kept and the `%20`
    // is not decoded into a space.
    assert_eq!(path_at(&state, &session, first).as_deref(), Some(FIRST));
    // The pane beside it announced nothing, and the targeted pane is what
    // decides: the neighbour answers for itself.
    assert_ne!(first, second);
    assert_eq!(path_at(&state, &session, second).as_deref(), Some(""));
    assert_eq!(
        modified_path_at(&state, &session, first),
        "[file://ppath-host/tmp/ppath%20one]|[ppath%20one]|[file://ppath-host/tmp]"
    );
    assert_eq!(modified_path_at(&state, &session, second), "[]|[.]|[.]");
}

#[tokio::test]
async fn a_second_announcement_replaces_the_first_on_the_same_pane() {
    let (mut state, session, first, second) = fixture("ppath-replaced");
    let index_before = pane_index(&state, &session, first);
    announce(&mut state, &session, first, FIRST);
    assert_eq!(path_at(&state, &session, first).as_deref(), Some(FIRST));

    announce(&mut state, &session, first, SECOND);
    // The last announcement is what the pane answers, and the one before it
    // designates this pane no longer.
    assert_eq!(path_at(&state, &session, first).as_deref(), Some(SECOND));
    assert_ne!(path_at(&state, &session, first).as_deref(), Some(FIRST));
    assert_eq!(
        modified_path_at(&state, &session, first),
        "[file://ppath-host/var/ppath%20two]|[ppath%20two]|[file://ppath-host/var]"
    );
    // The pane that answers is the one it always was, in the place it was.
    assert_eq!(pane_index(&state, &session, first), index_before);
    assert_eq!(path_at(&state, &session, second).as_deref(), Some(""));

    // A retained pane identity whose terminal was removed has no provider.
    assert!(state.remove_pane_terminal(&session, first));
    assert_eq!(
        context_at(&state, &session, first).pane_announced_path(),
        None
    );
    assert_eq!(modified_path_at(&state, &session, first), "[]|[]|[]");
    assert_eq!(modified_path_at(&state, &session, second), "[]|[.]|[.]");
}

#[tokio::test]
async fn a_pane_that_never_announced_answers_an_empty_path() {
    let (state, session, first, _second) = fixture("ppath-silent");

    // Empty, and empty on purpose: no working directory is put in its place.
    assert_eq!(path_at(&state, &session, first).as_deref(), Some(""));
    let session_path = value_at(&state, &session, first, "session_path");
    assert_eq!(
        session_path.as_deref(),
        Some(ELSEWHERE),
        "the fixture has a path in context to be confused with"
    );
    assert_ne!(path_at(&state, &session, first), session_path);
}

#[tokio::test]
async fn a_format_without_a_pane_answers_no_path_at_all() {
    let (mut state, session, first, _second) = fixture("ppath-absent");
    announce(&mut state, &session, first, FIRST);

    // A format that never reaches a pane has no path to give, and must not
    // borrow the one another pane announced.
    let view: ManagedSessionView<'_> = state.sessions.session(&session).expect("the session");
    let without_pane = RuntimeFormatContext::new(FormatContext::new())
        .with_state(&state)
        .with_session(view);
    assert_eq!(
        render_runtime_template(
            "[#{pane_path}]|[#{b:pane_path}]|[#{d:pane_path}]",
            &without_pane,
            false,
        ),
        "[]|[]|[]"
    );
    assert_eq!(without_pane.pane_announced_path(), None);
    let without_session = RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
    assert_eq!(
        render_runtime_template("[#{pane_path}]", &without_session, false),
        "[]"
    );
    let value = without_pane.format_value_by_name("pane_path");
    assert_ne!(value.as_deref(), Some(FIRST));
    // The provider itself answers nothing here. The empty string this reads is
    // the registered variable's terminal empty answer: the global same-name
    // sentinel cannot supply a value when this provider has none.
    assert_eq!(value.as_deref(), Some(""));
}

// D286: a live screen that has never received OSC 7 is a present empty path.
// Its two path modifiers are measured on tmux 3.7b; absence stays distinct.
#[tokio::test]
async fn silent_pane_path_modifiers_match_the_frozen_oracle_before_and_after_neighbour_osc7() {
    let (mut state, session, first, second) = fixture("ppath-frozen-oracle");
    assert_eq!(path_at(&state, &session, second).as_deref(), Some(""));
    assert_eq!(modified_path_at(&state, &session, second), "[]|[.]|[.]");
    announce(&mut state, &session, first, FIRST);
    assert_eq!(path_at(&state, &session, first).as_deref(), Some(FIRST));
    assert_eq!(modified_path_at(&state, &session, second), "[]|[.]|[.]");
    announce(&mut state, &session, first, SECOND);
    assert_eq!(path_at(&state, &session, first).as_deref(), Some(SECOND));
    assert_eq!(modified_path_at(&state, &session, second), "[]|[.]|[.]");
    assert!(state.remove_pane_terminal(&session, first));
    assert_eq!(modified_path_at(&state, &session, first), "[]|[]|[]");
    assert_eq!(modified_path_at(&state, &session, second), "[]|[.]|[.]");
}
