//! `allow-rename`: a pane's own `ESC k` output renames its canonical window.
//!
//! Every witness drives the real parser and the real runtime joint — pane
//! output is fed to the live transcript and the resulting append effects are
//! carried by a pane alert event exactly as the reader carries them — so what
//! is asserted is the server closure, not a mock `ScreenWriter`.
//!
//! Expectations come from the pinned tmux 3.7b sources (`input.c:2802-2831`,
//! `window.c:404-413`, `tmux.c:285-298`) and were measured against the pinned
//! binary in `arn1/oracle`.

use std::time::Duration;

use rmux_core::LifecycleEvent;
use rmux_proto::{
    KillWindowRequest, LinkWindowRequest, MovePaneRequest, NewSessionExtRequest, NewWindowRequest,
    OptionName, PaneTarget, Request, RespawnPaneRequest, Response, ScopeSelector, SessionName,
    SetOptionByNameRequest, SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowExtRequest,
    SplitWindowTarget, TerminalSize, WindowTarget,
};
use tokio::sync::broadcast;

use super::alert_support::pane_alert_coalescer::{PaneAlertFlushHold, PANE_ALERT_COALESCE_DELAY};
use super::{QueuedLifecycleEvent, RequestHandler};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
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

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
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
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
    session
}

async fn set_option(
    handler: &RequestHandler,
    scope: ScopeSelector,
    option: OptionName,
    value: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope,
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn allow_rename(handler: &RequestHandler, scope: ScopeSelector, value: &str) {
    set_option(handler, scope, OptionName::AllowRename, value).await;
}

/// A real `set -wu`, so the window entry is removed and the inherited value
/// takes over again — not a write of the default.
async fn unset_window_option(handler: &RequestHandler, target: &WindowTarget, name: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: rmux_proto::OptionScopeSelector::Window(target.clone()),
            name: name.to_owned(),
            value: None,
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: true,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
}

/// Splits with the same quiet command the session uses, so the second pane is
/// a known process and not the operator's own interactive shell — whose
/// prompt is free to emit terminal sequences of its own, including `ESC k`.
async fn split_window(handler: &RequestHandler, session: &SessionName) {
    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    let Response::SplitWindow(response) = response else {
        panic!("expected a split-window response, got {response:?}");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&response.pane)
        .await;
}

async fn pane_id_of(handler: &RequestHandler, target: &PaneTarget) -> rmux_core::PaneId {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(rmux_core::Pane::id)
        .expect("pane exists")
}

/// Publishes real pane output through the production path, with the server's
/// own effect sink.
///
/// Everything that decides the outcome is production: the parser, the owner
/// stamp taken at acceptance, the rename admission, the ordered sequence, the
/// alert callback and the flush it arms. Nothing about a rename is
/// hand-assembled here.
async fn publish(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    let (pane_id, generation, transcript, output) = {
        let state = handler.state.lock().await;
        let pane_id = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .and_then(|window| window.pane(target.pane_index()))
            .map(rmux_core::Pane::id)
            .expect("pane exists");
        let generation = state.pane_output_generation_for_target(target, pane_id);
        (
            pane_id,
            Some(generation),
            state
                .transcript_handle(target)
                .expect("pane has a live transcript"),
            state
                .pane_output_for_target(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )
                .expect("pane has a live output channel"),
        )
    };
    let effects = handler.pane_alert_callback();
    crate::pane_io::publish_pane_bytes_through_effects(
        target.session_name(),
        pane_id,
        &transcript,
        &output,
        generation,
        &effects,
        bytes.to_vec(),
    )
    .await;
}

/// Publishes and then waits past the coalescing delay the production callback
/// arms, so the flush that applies the sequence has run.
async fn emit(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    publish(handler, target, bytes).await;
    settle().await;
}

/// Publishes and then waits for the flush that takes what it queued, not for
/// a delay: see [`wait_for_rename_sequence_flush`].
async fn emit_through_flush(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    publish(handler, target, bytes).await;
    wait_for_rename_sequence_flush(handler).await;
}

async fn window_id_of(
    handler: &RequestHandler,
    target: &WindowTarget,
) -> Option<rmux_core::WindowId> {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .map(rmux_core::Window::id)
}

/// The owner the live pane would stamp on its next accepted request.
async fn transcript_rename_owner(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> Option<rmux_core::WindowId> {
    let state = handler.state.lock().await;
    let transcript = state
        .transcript_handle(target)
        .expect("pane has a live transcript");
    let owner = transcript.lock().rename_owner();
    owner
}

async fn window_name(handler: &RequestHandler, target: &WindowTarget) -> Option<String> {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(rmux_core::Window::name)
        .map(str::to_owned)
}

/// The window's own `automatic-rename` entry, tmux's `options_get_only`.
async fn local_automatic_rename(handler: &RequestHandler, target: &WindowTarget) -> Option<String> {
    let state = handler.state.lock().await;
    state
        .options
        .window_value(target, OptionName::AutomaticRename)
        .map(str::to_owned)
}

async fn resolved_automatic_rename(
    handler: &RequestHandler,
    target: &WindowTarget,
) -> Option<String> {
    let state = handler.state.lock().await;
    state
        .options
        .resolve_for_window(
            target.session_name(),
            target.window_index(),
            OptionName::AutomaticRename,
        )
        .map(str::to_owned)
}

async fn pane_title(handler: &RequestHandler, target: &PaneTarget) -> String {
    let state = handler.state.lock().await;
    let transcript = state
        .transcript_handle(target)
        .expect("pane has a live transcript");
    let title = transcript.lock().render_state().title;
    title
}

fn drain_lifecycle(
    events: &mut broadcast::Receiver<QueuedLifecycleEvent>,
) -> Vec<(LifecycleEvent, Option<String>)> {
    let mut drained = Vec::new();
    while let Ok(event) = events.try_recv() {
        let hook_name = event
            .formats
            .iter()
            .find_map(|(name, value)| (name == "hook_window_name").then(|| value.clone()));
        drained.push((event.event, hook_name));
    }
    drained
}

fn renamed_windows(drained: &[(LifecycleEvent, Option<String>)]) -> Vec<WindowTarget> {
    drained
        .iter()
        .filter_map(|(event, _)| match event {
            LifecycleEvent::WindowRenamed { target } => Some(target.clone()),
            _ => None,
        })
        .collect()
}

/// The window name each `window-renamed` hook was published with, in order.
fn renamed_names(drained: &[(LifecycleEvent, Option<String>)]) -> Vec<String> {
    drained
        .iter()
        .filter(|(event, _)| matches!(event, LifecycleEvent::WindowRenamed { .. }))
        .map(|(_, name)| name.clone().unwrap_or_default())
        .collect()
}

fn pane_titles_changed(drained: &[(LifecycleEvent, Option<String>)]) -> usize {
    drained
        .iter()
        .filter(|(event, _)| matches!(event, LifecycleEvent::PaneTitleChanged { .. }))
        .count()
}

/// Waits for a coalesced flush to publish `expected` rename events.
async fn wait_for_renames(
    target: &WindowTarget,
    expected: usize,
    events: &mut broadcast::Receiver<QueuedLifecycleEvent>,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut renamed = Vec::new();
    while renamed.len() < expected {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the flush published {} of {expected} window-renamed events for {target}: {renamed:?}",
            renamed.len()
        );
        renamed.extend(renamed_names(&drain_lifecycle(events)));
        if renamed.len() < expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    renamed
}

/// Waits past the production coalescing delay and its flush.
async fn settle() {
    tokio::time::sleep(PANE_ALERT_COALESCE_DELAY + Duration::from_millis(120)).await;
}

/// Waits for the flush that takes the rename sequence, rather than for a
/// guessed delay that a loaded host can outlast before the flush is polled.
///
/// Both the timed flush and a pane exit take the sequence and apply it under
/// one state lock, so a state read made after the sequence is seen empty is
/// ordered after that application. The timed flush also keeps the dispatch
/// lock until it has published its lifecycle events, so taking that lock
/// orders a lifecycle drain after them. A request that never leaves the
/// sequence is a defect of its own and fails here rather than passing as
/// "nothing was renamed".
async fn wait_for_rename_sequence_flush(handler: &RequestHandler) {
    tokio::time::timeout(
        Duration::from_secs(10),
        handler.pane_rename_queue_for_test().wait_until_empty(),
    )
    .await
    .expect("an accepted rename request leaves the sequence through a flush");
    drop(handler.pane_alert_dispatch.lock().await);
}

/// Holds the next timed flush past its coalescing delay and short of the
/// dispatch lock, so a witness that races it against a pane exit decides the
/// order instead of the host's scheduling.
fn hold_next_flush(handler: &RequestHandler) -> std::sync::Arc<PaneAlertFlushHold> {
    handler
        .pane_alert_coalescer
        .lock()
        .expect("pane alert coalescer mutex must not be poisoned")
        .hold_next_flush()
}

#[tokio::test]
async fn a_rename_request_is_refused_until_allow_rename_is_turned_on() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-gate").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    let baseline = window_name(&handler, &window).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(&handler, &pane, b"\x1bkrefused\x1b\\").await;
    settle().await;

    assert_eq!(window_name(&handler, &window).await, baseline);
    assert!(renamed_windows(&drain_lifecycle(&mut lifecycle)).is_empty());

    // Hot set: the live pane must obey without a respawn.
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(&handler, &pane, b"\x1bkaccepted\x1b\\").await;
    settle().await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("accepted")
    );
    assert_eq!(
        renamed_windows(&drain_lifecycle(&mut lifecycle)),
        vec![window.clone()],
        "exactly one window-renamed for one accepted request"
    );
    assert_eq!(
        local_automatic_rename(&handler, &window).await.as_deref(),
        Some("off"),
        "a non-empty request turns the window's own automatic-rename off"
    );

    // Hot unset: the option falls back to the window-global default `off`.
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(window.clone()),
            option: OptionName::AllowRename,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(&handler, &pane, b"\x1bkrefused-again\x1b\\").await;
    settle().await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("accepted")
    );
    assert!(renamed_windows(&drain_lifecycle(&mut lifecycle)).is_empty());
}

#[tokio::test]
async fn a_pane_override_decides_the_rename_for_that_pane_alone() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-pane-override").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "off").await;
    allow_rename(&handler, ScopeSelector::Pane(pane.clone()), "on").await;

    emit(&handler, &pane, b"\x1bkby-pane\x1b\\").await;
    settle().await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("by-pane"),
        "the pane override beats the window value"
    );
}

#[tokio::test]
async fn allow_set_title_does_not_gate_a_rename_and_a_rename_is_not_a_pane_title() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-titles").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;
    set_option(
        &handler,
        ScopeSelector::Window(window.clone()),
        OptionName::AllowSetTitle,
        "off",
    )
    .await;
    let title_before = pane_title(&handler, &pane).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(
        &handler,
        &pane,
        b"\x1b]2;blocked-title\x1b\\\x1bkallowed-name\x1b\\",
    )
    .await;
    settle().await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("allowed-name"),
        "allow-set-title off leaves ESC k alone"
    );
    assert_eq!(
        pane_title(&handler, &pane).await,
        title_before,
        "allow-set-title off still blocks OSC 2"
    );
    let drained = drain_lifecycle(&mut lifecycle);
    assert_eq!(renamed_windows(&drained), vec![window.clone()]);
    assert_eq!(
        pane_titles_changed(&drained),
        0,
        "ESC k must never publish pane-title-changed"
    );

    // The mirror image: titles allowed, renames refused.
    set_option(
        &handler,
        ScopeSelector::Window(window.clone()),
        OptionName::AllowSetTitle,
        "on",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "off").await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(
        &handler,
        &pane,
        b"\x1b]2;allowed-title\x1b\\\x1bkrefused-name\x1b\\",
    )
    .await;
    settle().await;

    assert_eq!(pane_title(&handler, &pane).await, "allowed-title");
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("allowed-name")
    );
    assert!(renamed_windows(&drain_lifecycle(&mut lifecycle)).is_empty());
}

#[tokio::test]
async fn an_empty_request_under_an_inherited_off_names_the_window_empty() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-empty-off").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;

    emit(&handler, &pane, b"\x1bkbefore-empty\x1b\\").await;
    settle().await;
    assert_eq!(
        local_automatic_rename(&handler, &window).await.as_deref(),
        Some("off")
    );
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(&handler, &pane, b"\x1bk\x1b\\").await;
    settle().await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some(""),
        "the inherited value is still off, so the empty name is the window's name"
    );
    assert_eq!(
        local_automatic_rename(&handler, &window).await,
        None,
        "the window's own automatic-rename override is removed, not set to off"
    );
    assert_eq!(
        resolved_automatic_rename(&handler, &window)
            .await
            .as_deref(),
        Some("off")
    );
    assert_eq!(
        renamed_windows(&drain_lifecycle(&mut lifecycle)),
        vec![window],
        "the empty name notifies exactly once"
    );
}

#[tokio::test]
async fn an_empty_request_under_an_inherited_on_restores_automatic_naming() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-empty-on").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "on",
    )
    .await;
    set_option(
        &handler,
        ScopeSelector::Window(window.clone()),
        OptionName::AutomaticRenameFormat,
        "arn1-automatic",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;

    emit(&handler, &pane, b"\x1bkbefore-empty\x1b\\").await;
    settle().await;
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("before-empty"),
        "the explicit name holds while the window's automatic-rename is off"
    );
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(&handler, &pane, b"\x1bk\x1b\\").await;
    settle().await;

    assert_eq!(
        local_automatic_rename(&handler, &window).await,
        None,
        "the override is removed"
    );
    assert_eq!(
        resolved_automatic_rename(&handler, &window)
            .await
            .as_deref(),
        Some("on")
    );
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("arn1-automatic"),
        "the automatic producer owns the name again"
    );
    assert_eq!(
        renamed_windows(&drain_lifecycle(&mut lifecycle)),
        vec![window],
        "the empty request itself names nothing; the automatic producer notifies once"
    );
}

#[tokio::test]
async fn an_initial_empty_request_and_repeated_names_each_reach_the_window() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-repeats").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    // The very first request is empty: a snapshot comparison would see nothing.
    emit(&handler, &pane, b"\x1bk\x1b\\").await;
    settle().await;

    assert_eq!(window_name(&handler, &window).await.as_deref(), Some(""));
    assert_eq!(
        renamed_windows(&drain_lifecycle(&mut lifecycle)),
        vec![window.clone()]
    );

    let mut lifecycle = handler.subscribe_lifecycle_events();
    emit(&handler, &pane, b"\x1bksame\x1b\\").await;
    settle().await;
    emit(&handler, &pane, b"\x1bksame\x1b\\").await;
    settle().await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("same")
    );
    assert_eq!(
        renamed_windows(&drain_lifecycle(&mut lifecycle)),
        vec![window.clone(), window],
        "a repeated identical name notifies again, as tmux's window_set_name does"
    );
}

#[tokio::test]
async fn an_untrusted_name_is_cleaned_and_an_invalid_one_is_ignored() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-clean").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;

    emit(&handler, &pane, b"\x1bka#(whoami)b\x1b\\").await;
    settle().await;
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("a_(whoami)b"),
        "`#(` is neutralised so a pane cannot make its window name run a command"
    );

    emit(&handler, &pane, b"\x1bkkeep#hash\x1b\\").await;
    settle().await;
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("keep#hash"),
        "a `#` that does not open a command substitution is kept"
    );

    let mut lifecycle = handler.subscribe_lifecycle_events();
    emit(&handler, &pane, b"\x1bkbad\xffname\x1b\\").await;
    settle().await;
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("keep#hash"),
        "an invalid name renames nothing at all"
    );
    assert!(renamed_windows(&drain_lifecycle(&mut lifecycle)).is_empty());

    emit(&handler, &pane, "\x1bkécran\x1b\\".as_bytes()).await;
    settle().await;
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("écran"),
        "a valid multi-byte name is kept verbatim"
    );
}

#[tokio::test]
async fn several_requests_of_one_read_are_applied_in_order() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-order").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    emit(&handler, &pane, b"\x1bkfirst\x1b\\text\x1bksecond\x1b\\").await;
    settle().await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("second"),
        "the last request of the chunk wins because every one is applied"
    );
    assert_eq!(
        renamed_windows(&drain_lifecycle(&mut lifecycle)),
        vec![window.clone(), window.clone()],
        "two requests in one read are two renames"
    );
}

#[tokio::test]
async fn a_rename_from_a_linked_inactive_window_names_the_whole_family() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-linked-alpha").await;
    let gamma = create_session(&handler, "arn1-linked-gamma").await;
    let alpha_window = WindowTarget::with_window(alpha.clone(), 0);
    let gamma_window = WindowTarget::with_window(gamma.clone(), 1);
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: alpha_window.clone(),
            target: gamma_window.clone(),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(alpha_window.clone()), "on").await;
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_id_before = pane_id_of(&handler, &pane).await;

    emit(&handler, &pane, b"\x1bklinked-name\x1b\\").await;
    settle().await;

    assert_eq!(
        window_name(&handler, &alpha_window).await.as_deref(),
        Some("linked-name")
    );
    assert_eq!(
        window_name(&handler, &gamma_window).await.as_deref(),
        Some("linked-name"),
        "the alias in the inactive slot of another session follows the canonical owner"
    );
    assert_eq!(
        pane_id_of(&handler, &pane).await,
        pane_id_before,
        "renaming a window preserves the pane identity that requested it"
    );
}

#[tokio::test]
async fn a_backslash_is_doubled_the_way_the_pinned_cleaner_does() {
    // Measured against the pinned tmux 3.7b in `arn1/oracle-r2`:
    // `clean_name` sends the name through `utf8_stravis`, whose `vis` doubles a
    // visible backslash because `VIS_NOSLASH` is not among tmux's flags.
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-backslash").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;

    // hex 61 5c 62.
    emit_through_flush(&handler, &pane, b"\x1bka\\b\x1b\\").await;

    assert_eq!(
        window_name(&handler, &window)
            .await
            .as_deref()
            .map(str::as_bytes),
        Some(b"a\\\\b".as_slice()),
        "hex 61 5c 62 must be stored as hex 61 5c 5c 62"
    );

    emit_through_flush(&handler, &pane, b"\x1bkc\\\\d\x1b\\").await;
    assert_eq!(
        window_name(&handler, &window)
            .await
            .as_deref()
            .map(str::as_bytes),
        Some(b"c\\\\\\\\d".as_slice()),
        "two backslashes become four"
    );

    emit_through_flush(&handler, &pane, b"\x1bke\\#(f\x1b\\").await;
    assert_eq!(
        window_name(&handler, &window)
            .await
            .as_deref()
            .map(str::as_bytes),
        Some(b"e\\\\_(f".as_slice()),
        "the `#(` neutralisation and the doubling both apply"
    );

    emit_through_flush(&handler, &pane, b"\x1bkg\"h\x1b\\").await;
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("g\"h"),
        "a double quote is copied unchanged: VIS_DQ is not among tmux's flags"
    );
}

#[tokio::test]
async fn two_panes_of_one_window_keep_the_order_their_requests_were_observed_in() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-two-panes").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    split_window(&handler, &alpha).await;
    let pane_a = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_b = PaneTarget::with_window(alpha.clone(), 0, 1);
    assert_ne!(
        pane_id_of(&handler, &pane_a).await,
        pane_id_of(&handler, &pane_b).await,
        "two distinct panes of one window"
    );
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Global, "on").await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    // Three publications observed as A1, B1, A2, all before any flush: the
    // dispatch lock the flush itself takes holds it off until they are in.
    {
        let _dispatch = handler.pane_alert_dispatch.lock().await;
        publish(&handler, &pane_a, b"\x1bkA1\x1b\\").await;
        publish(&handler, &pane_b, b"\x1bkB1\x1b\\").await;
        publish(&handler, &pane_a, b"\x1bkA2\x1b\\").await;
        assert_eq!(
            handler.pane_rename_queue_for_test().stats().pending,
            3,
            "all three are still waiting, so the flush sees the whole interleaving"
        );
    }

    let renamed = wait_for_renames(&window, 3, &mut lifecycle).await;

    assert_eq!(
        renamed,
        vec!["A1".to_owned(), "B1".to_owned(), "A2".to_owned()],
        "the hooks carry the three names in the order the pipeline observed them"
    );
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("A2"),
        "grouping by pane would have ended on B1; the observed order ends on A2"
    );
    assert_eq!(
        handler.pane_rename_queue_for_test().stats().pending,
        0,
        "the sequence is empty and its capacity returned"
    );
}

#[tokio::test]
async fn an_exit_applies_the_prefix_without_overtaking_another_pane() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-exit-order").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    split_window(&handler, &alpha).await;
    let pane_a = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_b = PaneTarget::with_window(alpha.clone(), 0, 1);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Global, "on").await;
    let pane_a_id = pane_id_of(&handler, &pane_a).await;
    let generation = {
        let state = handler.state.lock().await;
        Some(state.pane_output_generation_for_target(&pane_a, pane_a_id))
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();
    // Without the hold, a stall past the coalescing delay lets the ordinary
    // flush take the whole sequence before the exit runs.
    let flush = hold_next_flush(&handler);

    {
        let _dispatch = handler.pane_alert_dispatch.lock().await;
        publish(&handler, &pane_b, b"\x1bkB-first\x1b\\").await;
        publish(&handler, &pane_a, b"\x1bkA-last\x1b\\").await;
        publish(&handler, &pane_b, b"\x1bkB-after\x1b\\").await;
        assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 3);
    }

    // The exiting pane's own flush must carry what was accepted before it.
    handler
        .flush_pending_pane_alert_for_exit(pane_a_id, generation)
        .await;

    assert_eq!(
        renamed_names(&drain_lifecycle(&mut lifecycle)),
        vec!["B-first".to_owned(), "A-last".to_owned()],
        "the exit applies B-first and A-last, in that order"
    );
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("A-last"),
        "it does not overtake the request another pane accepted first"
    );
    assert_eq!(
        handler.pane_rename_queue_for_test().stats().pending,
        1,
        "what was accepted after the exiting generation stays for the ordinary flush"
    );

    flush.release();
    let renamed = wait_for_renames(&window, 1, &mut lifecycle).await;
    assert_eq!(renamed, vec!["B-after".to_owned()]);
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("B-after"),
        "and is applied later, not lost"
    );
}

#[tokio::test]
async fn a_full_sequence_holds_the_producer_and_resumes_it_without_loss() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-backpressure").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "on").await;

    // Three names are enough to fill the sequence, because the byte budget is
    // a bound of its own: a handful of very long names must stop a producer
    // that the count alone would still admit.
    let long = "x".repeat(100_000);
    let mut flood = Vec::new();
    for index in 0..3 {
        flood.extend_from_slice(format!("\x1bk{index}{long}\x1b\\").as_bytes());
    }
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let held = {
        // Hold the dispatch lock the flush itself takes, so the consumer is
        // genuinely late rather than merely slow.
        let _dispatch = handler.pane_alert_dispatch.lock().await;
        publish(&handler, &pane, &flood).await;
        let stats = handler.pane_rename_queue_for_test().stats();
        assert_eq!(stats.pending, 3, "nothing the feed accepted is dropped");
        assert_eq!(
            stats.pending_bytes, 300_003,
            "the sequence is over its byte budget, under its count budget"
        );

        // The next producer cannot even start: it is parked on capacity,
        // before it publishes anything.
        let held_handler = handler.clone();
        let held_pane = pane.clone();
        let held = tokio::spawn(async move {
            publish(&held_handler, &held_pane, b"\x1bkheld\x1b\\").await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!held.is_finished(), "the producer waits for room");
        assert_eq!(
            handler.pane_rename_queue_for_test().stats().pending,
            3,
            "and it published nothing while it waited"
        );
        held
    };

    let renamed = wait_for_renames(&window, 3, &mut lifecycle).await;
    assert_eq!(
        renamed.iter().map(|name| &name[..1]).collect::<Vec<_>>(),
        vec!["0", "1", "2"],
        "the three long names are applied in order"
    );
    tokio::time::timeout(Duration::from_secs(10), held)
        .await
        .expect("the held producer resumes once the sequence drains")
        .expect("the held producer finishes");

    let renamed = wait_for_renames(&window, 1, &mut lifecycle).await;
    assert_eq!(
        renamed,
        vec!["held".to_owned()],
        "the held request lands once: neither lost nor duplicated"
    );
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("held")
    );
    let stats = handler.pane_rename_queue_for_test().stats();
    assert_eq!(
        (stats.pending, stats.pending_bytes, stats.admitted),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn a_transfer_before_the_flush_leaves_the_request_with_its_old_owner() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-transfer").await;
    let source = WindowTarget::with_window(alpha.clone(), 0);
    let destination = WindowTarget::with_window(alpha.clone(), 1);
    // Window 0 keeps a pane after the move, so both windows stay alive and the
    // rejection is about the owner, not about a destroyed window.
    split_window(&handler, &alpha).await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("destination".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Global, "on").await;
    let moving = PaneTarget::with_window(alpha.clone(), 0, 1);
    let pane_id = pane_id_of(&handler, &moving).await;
    let generation = {
        let state = handler.state.lock().await;
        state.pane_output_generation_for_target(&moving, pane_id)
    };
    let source_name = window_name(&handler, &source).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    publish(&handler, &moving, b"\x1bkfrom-the-old-window\x1b\\").await;
    assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 1);

    let response = handler
        .handle(Request::MovePane(MovePaneRequest {
            source: moving.clone(),
            target: PaneTarget::with_window(alpha.clone(), 1, 0),
            direction: SplitDirection::Horizontal,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        }))
        .await;
    assert!(matches!(response, Response::MovePane(_)), "{response:?}");
    let moved = PaneTarget::with_window(alpha.clone(), 1, 1);
    assert_eq!(
        pane_id_of(&handler, &moved).await,
        pane_id,
        "the transfer preserves the pane id the request was published with"
    );
    assert_eq!(
        {
            let state = handler.state.lock().await;
            state.pane_output_generation_for_target(&moved, pane_id)
        },
        generation,
        "and its output generation"
    );
    // The transfer's own refresh really re-stamped the live pane: what it
    // accepts from now on belongs to the destination window.
    assert_eq!(
        transcript_rename_owner(&handler, &moved).await,
        window_id_of(&handler, &destination).await,
        "the moved pane's transcript now stamps the destination window"
    );

    wait_for_rename_sequence_flush(&handler).await;

    assert_eq!(
        window_name(&handler, &destination).await.as_deref(),
        Some("destination"),
        "a request accepted in the source window must not rename the destination"
    );
    assert_eq!(
        window_name(&handler, &source).await,
        source_name,
        "and the source window it no longer belongs to is not renamed either"
    );
    assert!(renamed_windows(&drain_lifecycle(&mut lifecycle)).is_empty());
    assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 0);
}

#[tokio::test]
async fn a_request_whose_pane_is_gone_renames_no_replacement_window() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-stale").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Global, "on").await;
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("survivor".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
            target_window_index: Some(1),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    let survivor = WindowTarget::with_window(alpha.clone(), 1);
    let survivor_name = window_name(&handler, &survivor).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    publish(&handler, &pane, b"\x1bkstale-pane\x1b\\").await;
    assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 1);
    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: window.clone(),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    wait_for_rename_sequence_flush(&handler).await;

    assert_eq!(
        window_name(&handler, &survivor).await,
        survivor_name,
        "a dead pane's request must not rename the window that replaced it"
    );
    assert!(renamed_windows(&drain_lifecycle(&mut lifecycle)).is_empty());
    assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 0);
}

#[tokio::test]
async fn a_real_unset_lets_an_inherited_value_reappear() {
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-unset").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    // The inherited value is on; the window overrides it to off.
    allow_rename(&handler, ScopeSelector::Global, "on").await;
    allow_rename(&handler, ScopeSelector::Window(window.clone()), "off").await;

    emit_through_flush(&handler, &pane, b"\x1bkrefused-by-window\x1b\\").await;
    let baseline = window_name(&handler, &window).await;
    assert_ne!(baseline.as_deref(), Some("refused-by-window"));

    // A real unset of the window entry, not a write of "off".
    unset_window_option(&handler, &window, "allow-rename").await;
    assert_eq!(
        {
            let state = handler.state.lock().await;
            state
                .options
                .window_value(&window, OptionName::AllowRename)
                .map(str::to_owned)
        },
        None,
        "the window's own entry is gone"
    );

    emit_through_flush(&handler, &pane, b"\x1bkinherited-on\x1b\\").await;

    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("inherited-on"),
        "the inherited value reappears and the live pane obeys it"
    );
}

#[tokio::test]
async fn a_pane_that_cannot_rename_never_charges_the_sequence() {
    // The admission is on the reader's own path, so a pane that can never
    // accept a rename must never be able to reach it. Nothing charges the
    // sequence while `allow-rename` is off, so no such pane can be throttled
    // by a full sequence, whatever another pane is doing.
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-inert").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    // The automatic producer is off, so the only rename this pane could
    // obtain is the refused one.
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    let baseline = window_name(&handler, &window).await;

    for index in 0..64 {
        publish(
            &handler,
            &pane,
            format!("\x1bkrefused-{index}\x1b\\some output\r\n").as_bytes(),
        )
        .await;
        let stats = handler.pane_rename_queue_for_test().stats();
        assert_eq!(
            (stats.pending, stats.pending_bytes, stats.admitted),
            (0, 0, 0),
            "a refused request charges nothing and leaks no admission"
        );
    }

    settle().await;

    assert_eq!(window_name(&handler, &window).await, baseline);
    let stats = handler.pane_rename_queue_for_test().stats();
    assert_eq!(
        (stats.pending, stats.pending_bytes, stats.admitted),
        (0, 0, 0)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_flush_cannot_apply_the_suffix_before_an_exit_applies_its_prefix() {
    // The real concurrency, forced: the exit is suspended exactly between
    // taking its prefix and applying it, and an ordinary flush is given its
    // chance at the suffix in that interval. Extraction and application are
    // one transaction, so the flush waits and the three names keep the order
    // the pipeline observed. Releasing the locks in that interval is what R2
    // did, and it lets the suffix take the earlier publication ranks.
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-exit-race").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    split_window(&handler, &alpha).await;
    let pane_a = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_b = PaneTarget::with_window(alpha.clone(), 0, 1);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Global, "on").await;
    let pane_a_id = pane_id_of(&handler, &pane_a).await;
    let generation = {
        let state = handler.state.lock().await;
        Some(state.pane_output_generation_for_target(&pane_a, pane_a_id))
    };
    let mut lifecycle = handler.subscribe_lifecycle_events();
    // The callback arms the ordinary flush on the first publication below, and
    // its delay runs from there. Held only by the dispatch lock, that flush
    // queues for the lock as soon as this task stalls past the delay, gets it
    // ahead of the exit, and applies the whole sequence before the exit
    // exists: the order is still right, but the race under test never happens.
    // The hold keeps it past its delay and short of the lock until the exit
    // holds its transaction open.
    let flush = hold_next_flush(&handler);

    // B-first, A-last, B-after, all accepted before anything is applied.
    {
        let _dispatch = handler.pane_alert_dispatch.lock().await;
        publish(&handler, &pane_b, b"\x1bkB-first\x1b\\").await;
        publish(&handler, &pane_a, b"\x1bkA-last\x1b\\").await;
        publish(&handler, &pane_b, b"\x1bkB-after\x1b\\").await;
        assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 3);
    }
    tokio::time::timeout(Duration::from_secs(5), flush.wait_until_reached())
        .await
        .expect("the ordinary flush is armed and past its coalescing delay");

    let pause = handler.install_pane_exit_rename_pause();
    let exit_handler = handler.clone();
    let exit = tokio::spawn(async move {
        exit_handler
            .flush_pending_pane_alert_for_exit(pane_a_id, generation)
            .await;
    });

    // The exit is now holding its transaction open at the boundary.
    tokio::time::timeout(Duration::from_secs(5), pause.wait_until_reached())
        .await
        .expect("the exit reaches its extraction/application boundary");
    assert_eq!(
        handler.pane_rename_queue_for_test().stats().pending,
        1,
        "the prefix has left the sequence and only the suffix remains"
    );

    // Give the ordinary flush every chance to apply that suffix first: it is
    // already past its coalescing delay and now asks for the dispatch lock.
    // Nothing here touches the handler state: the exit holds it, and that is
    // precisely the property under test - an observer that could read the
    // window name in this interval would prove the transaction was not held at
    // all.
    flush.release();
    tokio::time::sleep(PANE_ALERT_COALESCE_DELAY * 4).await;
    // Observed now, asserted at the end, so that a run which lets the flush
    // through fails on the order it produced rather than stopping here.
    let pending_during_the_pause = handler.pane_rename_queue_for_test().stats().pending;

    pause.release();
    tokio::time::timeout(Duration::from_secs(10), exit)
        .await
        .expect("the exit finishes")
        .expect("the exit task does not panic");

    let renamed = wait_for_renames(&window, 3, &mut lifecycle).await;
    assert_eq!(
        renamed,
        vec![
            "B-first".to_owned(),
            "A-last".to_owned(),
            "B-after".to_owned()
        ],
        "the exit's prefix keeps the ranks it observed first, and the suffix follows"
    );
    assert_eq!(
        window_name(&handler, &window).await.as_deref(),
        Some("B-after"),
        "the window ends on the last request of the observed order"
    );
    assert_eq!(
        pending_during_the_pause, 1,
        "and the suffix was still waiting throughout: the flush never got past the exit"
    );
    assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 0);
}

#[tokio::test]
async fn a_request_from_a_superseded_generation_renames_nothing() {
    // The short stale-generation case R1 covered with a hand-written event and
    // that the R2 rewrite dropped. It is restored on the production path: the
    // pane is respawned while its request waits, so the generation the request
    // was published with is no longer the live one.
    let handler = RequestHandler::new();
    let alpha = create_session(&handler, "arn1-stale-generation").await;
    let window = WindowTarget::with_window(alpha.clone(), 0);
    let pane = PaneTarget::with_window(alpha.clone(), 0, 0);
    set_option(
        &handler,
        ScopeSelector::Global,
        OptionName::AutomaticRename,
        "off",
    )
    .await;
    allow_rename(&handler, ScopeSelector::Global, "on").await;
    let pane_id = pane_id_of(&handler, &pane).await;
    let generation_before = {
        let state = handler.state.lock().await;
        state.pane_output_generation_for_target(&pane, pane_id)
    };
    let baseline = window_name(&handler, &window).await;
    let mut lifecycle = handler.subscribe_lifecycle_events();

    publish(&handler, &pane, b"\x1bkfrom-the-old-generation\x1b\\").await;
    assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 1);

    let response = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: pane.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_command()),
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::RespawnPane(_)), "{response:?}");
    let pane_id_after = pane_id_of(&handler, &pane).await;
    let generation_after = {
        let state = handler.state.lock().await;
        state.pane_output_generation_for_target(&pane, pane_id_after)
    };
    assert_ne!(
        generation_after, generation_before,
        "the respawn really superseded the generation the request carries"
    );

    wait_for_rename_sequence_flush(&handler).await;

    assert_eq!(
        window_name(&handler, &window).await,
        baseline,
        "a request published under a superseded generation renames nothing"
    );
    assert!(renamed_windows(&drain_lifecycle(&mut lifecycle)).is_empty());
    assert_eq!(handler.pane_rename_queue_for_test().stats().pending, 0);
}
