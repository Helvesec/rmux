//! WNAME1 — the window loop reads its own argument, and the inventory it renders
//! is what everything downstream follows.
//!
//! Every expectation below is a literal measured once on this parcours' own new
//! tmux 3.7b oracle, in one 80x24 fixture whose windows are `zeta` at index 0
//! and `alpha` at index 1, with `zeta` current
//! (`wname1/native/oracle-wname1b.json`, cell `w01`). The source
//! behind them: `format.c:5200-5219` takes `i` first, then `n`, then `t`, else
//! the session's own order, with `r` reversing the choice; `sort.c:29-51` never
//! sorts the session's own order at all, only reverses the collected list;
//! `sort.c:277-279` compares names with `strcmp`; and `format.c:4758-4787`
//! sorts BEFORE the loop, so the active body, the last flag and each item's own
//! context all follow the rendered order.

use super::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;
use rmux_core::formats::{FormatContext, FormatVariables};
use rmux_proto::{SessionName, TerminalSize, WindowTarget};

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const BODY: &str = "#{window_name}|#{window_id}|#{loop_last_flag};";

/// `zeta` at index 0 and `alpha` at index 1, with `zeta` current: the fixture
/// the oracle measured, built through the store's own constructors.
fn fixture(name: &str) -> (HandlerState, SessionName) {
    let mut state = HandlerState::default();
    let session_name = SessionName::new(name).expect("valid session name");
    state
        .sessions
        .create_session(session_name.clone(), SIZE)
        .expect("session creates");
    state
        .sessions
        .rename_window(
            &WindowTarget::with_window(session_name.clone(), 0),
            "zeta".to_owned(),
        )
        .expect("the first window is named");
    let created = state
        .sessions
        .create_window(&session_name, SIZE)
        .expect("the second window is created");
    assert_eq!(created.window_index, 1, "the second window takes index 1");
    state
        .sessions
        .rename_window(
            &WindowTarget::with_window(session_name.clone(), 1),
            "alpha".to_owned(),
        )
        .expect("the second window is named");
    (state, session_name)
}

/// The identities the fixture really produced, never assumed from an index.
fn identities(state: &HandlerState, session_name: &SessionName) -> (String, String) {
    let session = state
        .sessions
        .session(session_name)
        .expect("session exists");
    let zeta = session.window_at(0).expect("zeta exists").id().to_string();
    let alpha = session.window_at(1).expect("alpha exists").id().to_string();
    (zeta, alpha)
}

fn render(state: &HandlerState, session_name: &SessionName, template: &str) -> String {
    let session = state
        .sessions
        .session(session_name)
        .expect("session exists");
    let runtime = RuntimeFormatContext::new(FormatContext::new())
        .with_state(state)
        .with_session(session);
    render_runtime_template(template, &runtime, false)
}

#[test]
fn a_window_loop_sorted_by_name_renders_alpha_before_zeta() {
    let (state, session_name) = fixture("wname-sorted");
    let (zeta, alpha) = identities(&state, &session_name);
    assert_eq!(
        render(&state, &session_name, &format!("#{{Wn:{BODY}}}")),
        format!("alpha|{alpha}|0;zeta|{zeta}|1;"),
        "the names decide the order, and the last flag belongs to the last one rendered"
    );
}

#[test]
fn the_reverse_argument_turns_the_sorted_inventory_around() {
    let (state, session_name) = fixture("wname-reversed");
    let (zeta, alpha) = identities(&state, &session_name);
    assert_eq!(
        render(&state, &session_name, &format!("#{{Wnr:{BODY}}}")),
        format!("zeta|{zeta}|0;alpha|{alpha}|1;"),
        "reversed, with the flag still on whichever window is rendered last"
    );
}

#[test]
fn a_window_loop_without_an_argument_stays_in_the_session_order() {
    let (state, session_name) = fixture("wname-default");
    let (zeta, alpha) = identities(&state, &session_name);
    let expected = format!("zeta|{zeta}|0;alpha|{alpha}|1;");
    assert_eq!(
        render(&state, &session_name, &format!("#{{W:{BODY}}}")),
        expected,
        "no argument is the session's own order, by index"
    );
    assert_eq!(
        render(&state, &session_name, &format!("#{{Wi:{BODY}}}")),
        expected,
        "and `i` asks for that same order explicitly"
    );
}

#[test]
fn reversing_the_session_order_turns_the_list_around_without_sorting_it() {
    let (state, session_name) = fixture("wname-index-reversed");
    let (zeta, alpha) = identities(&state, &session_name);
    assert_eq!(
        render(&state, &session_name, &format!("#{{Wr:{BODY}}}")),
        format!("alpha|{alpha}|0;zeta|{zeta}|1;"),
        "the indexes are walked backwards; nothing is compared"
    );
}

#[test]
fn each_window_keeps_its_own_context_wherever_the_sort_puts_it() {
    let (state, session_name) = fixture("wname-context");
    let (zeta, alpha) = identities(&state, &session_name);
    assert_eq!(
        render(
            &state,
            &session_name,
            "#{Wn:#{window_index}/#{window_id}/#{window_name}/#{window_active}/#{window_width}x#{window_height};}",
        ),
        format!("1/{alpha}/alpha/0/80x24;0/{zeta}/zeta/1/80x24;"),
        "the index, the identity, the size and which one is current stay the window's own"
    );
}

#[test]
fn the_current_window_takes_the_active_body_wherever_the_sort_puts_it() {
    let (state, session_name) = fixture("wname-active-body");
    assert_eq!(
        render(
            &state,
            &session_name,
            "#{W:#{window_name};,CURRENT-#{window_name};}",
        ),
        "CURRENT-zeta;alpha;",
        "in the session's own order the current window is the first item"
    );
    assert_eq!(
        render(
            &state,
            &session_name,
            "#{Wn:#{window_name};,CURRENT-#{window_name};}",
        ),
        "alpha;CURRENT-zeta;",
        "and sorting moves it without changing which body it takes"
    );
}

#[test]
fn a_window_loop_answers_nothing_when_this_context_has_no_session() {
    let mut state = HandlerState::default();
    let session_name = SessionName::new("wname-absent").expect("valid session name");
    state
        .sessions
        .create_session(session_name.clone(), SIZE)
        .expect("session creates");
    let runtime = RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
    assert_eq!(
        runtime.format_loop_with_args('W', &["n".to_owned()], BODY, None, false),
        None,
        "a sorted loop with no session of its own answers nothing, exactly as the unsorted one did"
    );
    assert_eq!(
        runtime.format_loop_with_args('W', &[], BODY, None, false),
        None,
        "and so does the loop without an argument"
    );
}
