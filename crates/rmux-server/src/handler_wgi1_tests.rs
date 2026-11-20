//! `#{window_linked_sessions_list}` names a window's links in the order they were made,
//! including an outsider linked between a creation and a JOIN, and says so only
//! while this runtime can still vouch for that order.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux walks the WINDOW's own winlinks and prints one session name per
//! winlink (`format.c:686-714`); each is appended at the tail of that list as it
//! is made (`window.c:183-193`); removing a SESSION takes its own links away and
//! nothing else (`session.c:195-229`, which removes the session from its group
//! and then only its own winlinks); and three mutations instead REBUILD winlinks
//! wholesale, which moves them to the tail of lists this runtime does not model:
//! a renumber (`session.c:691-746`), and the group synchronisation a partial
//! detach triggers (`session.c:340-358` -> `:612-626` -> `:633-689`).
//!
//! Measured on WGI1 R1's tmux 3.7b image, LC_ALL=C, in ONE oracle cell with two
//! readers, BEFORE any witness of this lot was written
//! (`wgi1/cells/o1/STATES.json`); this complement builds no oracle
//! and adopts those five states under the SOURCE equivalence recorded in
//! `wgi1/r2/EQUIVALENCE.json`. `E` was created with reader A in its
//! own window 0; `zulu` with reader B in ITS window 0; that window was linked
//! explicitly into `E:1`; and only then `alpha` joined `zulu`. The five states:
//!
//!   before the removal      `zulu,E,alpha`   - the outsider in the MIDDLE
//!   after removing `zulu`   `E,alpha`
//!   after renaming E        `zz-E,alpha`
//!   a second alias at :2    `zz-E,alpha,zz-E`  - the duplicate is kept
//!   the old alias removed   `alpha,zz-E`     - the link made after the JOIN
//!                                              stayed where it was made
//!
//! Every fixture here is built through the REAL runtime wrappers - `new-session`,
//! `new-session -t`, `new-window`, `HandlerState::link_window`, `unlink_window`,
//! `kill_window`, `move_window -r`, `rename-session`, `kill-session` - and every
//! reading is a real `display-message -p -F`. Nothing installs a core alias
//! behind the runtime's back, so the explicit link groups exist and the
//! presentation accessor is exercised where it really answers.
//!
//! Scope of each witness, stated plainly. MEASURED: the five states above, held
//! by the first four witnesses. DEDUCED, and named as such where it is claimed:
//! the inverse history, the rollbacks, the renumber and the grouped partial
//! detach, which the oracle cell did not perform; each is derived from the lines
//! of the primary text quoted on it. The renumber and the grouped detach are
//! deliberately NOT modelled: the witnesses assert that this runtime stops
//! claiming an order it can no longer follow, not that it reconstructs tmux's.

use super::RequestHandler;
use rmux_proto::{
    DisplayMessageRequest, KillSessionRequest, KillWindowRequest, LinkWindowRequest,
    MoveWindowRequest, MoveWindowTarget, NewSessionExtRequest, NewWindowRequest,
    RenameSessionRequest, Request, Response, SessionName, Target, TerminalSize, WindowTarget,
};

use crate::pane_terminals::HandlerState;

// Windows installs a pane terminal AFTER `new-session` and `new-window` have
// returned: the session is created with a deferred, still-starting terminal and
// the pane's entry appears only once that startup finishes. The very next
// `link-window` asks `ensure_panes_exist` for that entry, and `kill-window`
// asks `ensure_session_contains_panes` for it, so both need the startup to have
// finished first. No other platform installs a terminal late, and nothing of
// the product, the assertions or the quiet producer changes here.
#[cfg(windows)]
use rmux_proto::PaneTarget;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const LIST: &str = "#{window_linked_sessions_list}";

fn name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn window(session: &SessionName, index: u32) -> WindowTarget {
    WindowTarget::with_window(session.clone(), index)
}

#[cfg(unix)]
fn quiet() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    [
        cmd.to_string_lossy().into_owned(),
        "/c".to_owned(),
        "timeout /t 60 /nobreak > NUL".to_owned(),
    ]
    .into_iter()
    .collect()
}

fn session_request(value: &str, group_target: Option<SessionName>) -> NewSessionExtRequest {
    // tmux refuses a command together with `-t` (`cmd-new-session.c`), and so
    // does this runtime: the JOIN takes the template's windows, it does not
    // spawn one of its own.
    let command = group_target.is_none().then(quiet);
    NewSessionExtRequest {
        session_name: Some(name(value)),
        working_directory: None,
        detached: true,
        size: Some(SIZE),
        environment: None,
        group_target,
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name: None,
        print_session_info: false,
        print_format: None,
        command,
        process_command: None,
        client_environment: None,
        skip_environment_update: false,
    }
}

/// `new-session -d -s <value>`.
async fn created(handler: &RequestHandler, value: &str) -> SessionName {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(session_request(
            value, None,
        ))))
        .await;
    crate::session_fixture::assert_new_session(&response);
    let session = name(value);
    // The pane this session really spawns, waited for with the runtime's own
    // bounded helper - terminal active AND no longer starting, 15s, polled
    // every 25ms - exactly as `handler_winlink_insertion_tests::create_session`
    // waits. The JOIN below spawns nothing of its own, so it waits for nothing.
    #[cfg(windows)]
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 0))
        .await;
    session
}

/// `new-session -d -s <value> -t <group>`: the JOIN.
async fn joined(handler: &RequestHandler, value: &str, group: &SessionName) -> SessionName {
    let response = handler
        .handle(Request::NewSessionExt(Box::new(session_request(
            value,
            Some(group.clone()),
        ))))
        .await;
    crate::session_fixture::assert_new_session(&response);
    name(value)
}

/// `new-window -d -t <session>:<index>`.
async fn opened(handler: &RequestHandler, session: &SessionName, index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            environment: None,
            command: Some(quiet()),
            start_directory: None,
            target_window_index: Some(index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    // The same deferred startup, for the window this opens: the grouped detach
    // witness kills it, and `kill-window` requires its pane terminal entry.
    #[cfg(windows)]
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session.clone(),
            index,
            0,
        ))
        .await;
}

/// `link-window [-k] -s <source> -t <target>`, through the runtime entry the
/// review named, with its real attach and its real rollback.
async fn linked(
    handler: &RequestHandler,
    source: WindowTarget,
    target: WindowTarget,
    kill_destination: bool,
) {
    let mut state = handler.state.lock().await;
    state
        .link_window(LinkWindowRequest {
            source,
            target,
            after: false,
            before: false,
            kill_destination,
            detached: true,
        })
        .expect("the link is made");
}

/// The same entry, with the post-attach failure injected: the transaction rolls
/// itself back through the structural checkpoint.
async fn link_fails_after_attach(
    handler: &RequestHandler,
    source: WindowTarget,
    target: WindowTarget,
    kill_destination: bool,
) -> String {
    let mut state = handler.state.lock().await;
    state.fail_next_link_window_after_attach_for_test();
    let error = state
        .link_window(LinkWindowRequest {
            source,
            target,
            after: false,
            before: false,
            kill_destination,
            detached: true,
        })
        .expect_err("the injected post-attach failure refuses the link");
    error.to_string()
}

/// `unlink-window -t <target>`.
async fn unlinked(handler: &RequestHandler, target: WindowTarget) {
    let mut state = handler.state.lock().await;
    state
        .unlink_window(target, false)
        .expect("the occurrence unlinks");
}

/// `kill-window -t <target>`.
async fn killed_window(handler: &RequestHandler, target: WindowTarget) {
    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target,
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");
}

/// `kill-session -t <target>`.
async fn killed_session(handler: &RequestHandler, target: &SessionName) {
    let response = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: target.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
}

/// `rename-session -t <from> <to>`.
async fn renamed(handler: &RequestHandler, from: &SessionName, to: &str) -> SessionName {
    let response = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: from.clone(),
            new_name: name(to),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    name(to)
}

/// `move-window -r -t <session>`: the renumber.
async fn renumbered(handler: &RequestHandler, session: &SessionName) {
    let mut state = handler.state.lock().await;
    state
        .move_window(MoveWindowRequest {
            source: None,
            target: MoveWindowTarget::Session(session.clone()),
            renumber: true,
            kill_destination: false,
            detached: true,
            after: false,
            before: false,
        })
        .expect("the session renumbers");
}

/// `display-message -p -t <target> -F '#{window_linked_sessions_list}'`.
async fn names(handler: &RequestHandler, target: WindowTarget) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Window(target)),
            print: true,
            message: Some(LIST.to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected a display-message response: {response:?}");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    String::from_utf8(output.stdout().to_vec())
        .expect("the expansion is utf-8")
        .trim_end()
        .to_owned()
}

/// Every session's slots, the identity each holds and the panes under it.
fn structure(state: &HandlerState) -> Vec<(String, u32, u32, Vec<u32>)> {
    let mut rows = state
        .sessions
        .iter()
        .flat_map(|session| {
            let session_name = session.name().as_str().to_owned();
            session
                .windows()
                .map(|(index, window)| {
                    (
                        session_name.clone(),
                        index,
                        window.id().as_u32(),
                        window
                            .panes()
                            .iter()
                            .map(|pane| pane.id().as_u32())
                            .collect(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

/// What the callers that WALK the links read, and what the aggregators read.
fn helpers(state: &HandlerState, target: &WindowTarget) -> (Vec<String>, Vec<String>, usize) {
    let session_name = target.session_name();
    let index = target.window_index();
    (
        state
            .window_linked_sessions_list(session_name, index)
            .into_iter()
            .map(|entry| entry.as_str().to_owned())
            .collect(),
        state
            .window_linked_session_family_list(session_name, index)
            .into_iter()
            .map(|entry| entry.as_str().to_owned())
            .collect(),
        state.window_linked_session_count(session_name, index),
    )
}

async fn shutdown(handler: &RequestHandler) {
    let mut state = handler.state.lock().await;
    state.shutdown_terminals_for_test();
}

/// The journey: `E` exists first with its own window; `zulu` then makes the
/// window this lot is about; that window is linked explicitly into `E:1`; and
/// only then does `alpha` join `zulu`.
struct Intercalated {
    outer: SessionName,
    creator: SessionName,
    peer: SessionName,
}

async fn intercalated(handler: &RequestHandler, label: &str) -> Intercalated {
    let outer = created(handler, &format!("{label}-E")).await;
    let creator = created(handler, &format!("{label}-zulu")).await;
    linked(handler, window(&creator, 0), window(&outer, 1), false).await;
    let peer = joined(handler, &format!("{label}-alpha"), &creator).await;
    Intercalated {
        outer,
        creator,
        peer,
    }
}

#[tokio::test]
async fn an_intercalated_link_is_named_between_the_creation_and_the_join() {
    let handler = RequestHandler::new();
    let world = intercalated(&handler, "wgi-middle").await;
    let expected = format!(
        "{},{},{}",
        world.creator.as_str(),
        world.outer.as_str(),
        world.peer.as_str()
    );
    assert_eq!(names(&handler, window(&world.peer, 0)).await, expected);
    assert_eq!(names(&handler, window(&world.creator, 0)).await, expected);
    assert_eq!(names(&handler, window(&world.outer, 1)).await, expected);
    // Removing the creator takes its own links away and leaves the rest in
    // place: `session_destroy` leaves the group and then removes only its own
    // winlinks, so no survivor's list is rebuilt (`session.c:195-229`).
    killed_session(&handler, &world.creator).await;
    let after = format!("{},{}", world.outer.as_str(), world.peer.as_str());
    assert_eq!(names(&handler, window(&world.peer, 0)).await, after);
    assert_eq!(names(&handler, window(&world.outer, 1)).await, after);
    shutdown(&handler).await;
}

#[tokio::test]
async fn the_same_sessions_with_the_link_made_after_the_join_are_named_the_other_way() {
    // DEDUCED, not measured: the oracle cell built one history. This is the
    // other one, from the same two lines of the primary text - a link appended
    // when it is made, whenever that is.
    let handler = RequestHandler::new();
    let outer = created(&handler, "wgi-after-E").await;
    let creator = created(&handler, "wgi-after-zulu").await;
    let peer = joined(&handler, "wgi-after-alpha", &creator).await;
    linked(&handler, window(&creator, 0), window(&outer, 1), false).await;
    assert_eq!(
        names(&handler, window(&peer, 0)).await,
        format!("{},{},{}", creator.as_str(), peer.as_str(), outer.as_str())
    );
    shutdown(&handler).await;
}

#[tokio::test]
async fn a_rename_leaves_the_link_where_it_was() {
    let handler = RequestHandler::new();
    let world = intercalated(&handler, "wgi-rename").await;
    killed_session(&handler, &world.creator).await;
    let renamed_outer = renamed(&handler, &world.outer, "wgi-rename-zz").await;
    assert_eq!(
        names(&handler, window(&world.peer, 0)).await,
        format!("{},{}", renamed_outer.as_str(), world.peer.as_str())
    );
    shutdown(&handler).await;
}

#[tokio::test]
async fn a_second_alias_is_named_again_and_removing_the_old_one_leaves_it_where_it_was() {
    let handler = RequestHandler::new();
    let world = intercalated(&handler, "wgi-second").await;
    killed_session(&handler, &world.creator).await;
    let outer = renamed(&handler, &world.outer, "wgi-second-zz").await;
    linked(&handler, window(&world.peer, 0), window(&outer, 2), false).await;
    // One name per link: the duplicate is kept, at the tail.
    assert_eq!(
        names(&handler, window(&world.peer, 0)).await,
        format!(
            "{},{},{}",
            outer.as_str(),
            world.peer.as_str(),
            outer.as_str()
        )
    );
    unlinked(&handler, window(&outer, 1)).await;
    assert_eq!(
        names(&handler, window(&world.peer, 0)).await,
        format!("{},{}", world.peer.as_str(), outer.as_str())
    );
    shutdown(&handler).await;
}

#[tokio::test]
async fn a_rolled_back_link_leaves_the_order_it_found() {
    // DEDUCED, not measured: the oracle cell performed no rollback. The claim is
    // that an undone change leaves the order it found, not a rebuilt one - here
    // for the link this transaction APPENDS, which the next witness completes
    // with the link it REPLACES.
    let handler = RequestHandler::new();
    let world = intercalated(&handler, "wgi-undo").await;
    killed_session(&handler, &world.creator).await;
    let before = names(&handler, window(&world.peer, 0)).await;
    assert_eq!(
        before,
        format!("{},{}", world.outer.as_str(), world.peer.as_str())
    );
    let error = link_fails_after_attach(
        &handler,
        window(&world.peer, 0),
        window(&world.outer, 3),
        false,
    )
    .await;
    assert!(error.contains("post-attach failure"), "{error}");
    assert_eq!(names(&handler, window(&world.peer, 0)).await, before);
    shutdown(&handler).await;
}

#[tokio::test]
async fn a_replaced_earlier_link_comes_back_where_it_was() {
    // DEDUCED, not measured, and the case the append-only rollback cannot tell
    // apart: the link this transaction takes away is the FIRST of the window's
    // three, not its last. Rebuilding the order from the restored tables puts it
    // back at the tail; only an order captured and restored as it was keeps it
    // where it was made.
    let handler = RequestHandler::new();
    let world = intercalated(&handler, "wgi-replace").await;
    let outsider = created(&handler, "wgi-replace-beta").await;
    let expected = format!(
        "{},{},{}",
        world.creator.as_str(),
        world.outer.as_str(),
        world.peer.as_str()
    );
    assert_eq!(names(&handler, window(&world.peer, 0)).await, expected);
    let (before, walked, family, count) = {
        let state = handler.state.lock().await;
        let (walked, family, count) = helpers(&state, &window(&world.peer, 0));
        (structure(&state), walked, family, count)
    };
    // `link-window -k -s beta:0 -t E:1` takes E:1 away from the shared window
    // and gives it to beta's; the injected failure then undoes all of it.
    let error = link_fails_after_attach(
        &handler,
        window(&outsider, 0),
        window(&world.outer, 1),
        true,
    )
    .await;
    assert!(error.contains("post-attach failure"), "{error}");
    assert_eq!(names(&handler, window(&world.peer, 0)).await, expected);
    assert_eq!(names(&handler, window(&world.outer, 1)).await, expected);
    assert_eq!(
        names(&handler, window(&outsider, 0)).await,
        outsider.as_str()
    );
    let state = handler.state.lock().await;
    // No trace on the links, on the original allocations, or on the helpers the
    // rest of the runtime walks.
    assert_eq!(structure(&state), before);
    assert_eq!(
        helpers(&state, &window(&world.peer, 0)),
        (walked, family, count)
    );
    drop(state);
    shutdown(&handler).await;
}

#[tokio::test]
async fn an_order_the_attempt_lost_comes_back_and_one_already_unknown_stays_unknown() {
    // DEDUCED, not measured. Two claims about the same rollback. A window whose
    // order the failing attempt could no longer follow gets back exactly the one
    // the checkpoint captured. And a window whose order was ALREADY unknown when
    // the checkpoint was taken comes back unknown: the rollback restores the
    // complete optional state, and invents nothing for the reader that must fall
    // back.
    let handler = RequestHandler::new();
    let world = intercalated(&handler, "wgi-lost").await;
    let expected = format!(
        "{},{},{}",
        world.creator.as_str(),
        world.outer.as_str(),
        world.peer.as_str()
    );
    // A group of two, an outsider's window linked into it, and then a renumber
    // of the group: the renumber rebuilds the group's winlinks, which this store
    // does not model, so it keeps no order for the outsider's window.
    let template = created(&handler, "wgi-lost-g1").await;
    let member = joined(&handler, "wgi-lost-g2", &template).await;
    let stranger = created(&handler, "wgi-lost-beta").await;
    linked(&handler, window(&stranger, 0), window(&template, 1), false).await;
    renumbered(&handler, &template).await;
    let unknown = names(&handler, window(&stranger, 0)).await;
    assert_eq!(
        unknown,
        format!(
            "{},{},{}",
            stranger.as_str(),
            template.as_str(),
            member.as_str()
        ),
        "with no order to vouch for, the reader falls back to the family list"
    );
    // The failing attempt links the SHARED window into that same group, which
    // changes its order for the duration.
    let error = link_fails_after_attach(
        &handler,
        window(&world.creator, 0),
        window(&template, 2),
        false,
    )
    .await;
    assert!(error.contains("post-attach failure"), "{error}");
    // The one that was already unknown is still unknown, and still falls back.
    assert_eq!(names(&handler, window(&stranger, 0)).await, unknown);
    // The one the attempt lost is back exactly as it was made.
    assert_eq!(names(&handler, window(&world.peer, 0)).await, expected);
    assert_eq!(names(&handler, window(&world.outer, 1)).await, expected);
    shutdown(&handler).await;
}

#[path = "handler_wgi1_tests/removal.rs"]
mod removal;
