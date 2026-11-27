//! Regression coverage for issue #182: with `set-titles on` an attached
//! client must carry the expanded `set-titles-string` toward its outer
//! terminal.
//!
//! These run at the layer where the title was missing — the attach render
//! frame handed to the client — rather than at the capability layer, so they
//! fail if any step of option → format expansion → prelude → refresh is lost.
//!
//! Oracle: tmux 3.7b, measured on this host. `set-titles off` writes neither
//! OSC 0 nor OSC 7; `set-titles on` writes OSC 0 carrying the expanded
//! `set-titles-string`, once per distinct value.

use super::set_titles_support::{
    append_global, client_title_of, new_detached_session, set_global, title_capable_context,
    titles_in, TITLE_OPEN,
};
use super::*;

/// The reporter's configuration: a custom `set-titles-string` must reach the
/// outer terminal expanded. Before the fix the frame carried only the bare
/// pane title, so `RMUXTEST` never appeared.
#[tokio::test]
async fn set_titles_on_carries_the_expanded_custom_string() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(&handler, OptionName::SetTitlesString, "RMUXTEST #S:#I").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    let target = recv_switch_target(&mut control_rx, "set-titles on refresh").await;
    assert_eq!(
        titles_in(&target.render_frame),
        vec!["RMUXTEST alpha:0".to_owned()],
        "the attach frame must carry the expanded set-titles-string"
    );
    assert_eq!(
        client_title_of(&target),
        Some("RMUXTEST alpha:0"),
        "the resolved title is reported back for per-client dedup"
    );
}

/// The default `set-titles-string` is `#S:#I:#W - "#T" #{session_alerts}`, so
/// the default configuration must expand it rather than emit a raw pane title.
#[tokio::test]
async fn set_titles_on_expands_the_default_string() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(&handler, OptionName::SetTitles, "on").await;

    let target = recv_switch_target(&mut control_rx, "set-titles on refresh").await;
    let titles = titles_in(&target.render_frame);
    assert_eq!(titles.len(), 1, "exactly one title, got {titles:?}");
    assert!(
        titles[0].starts_with("alpha:0:"),
        "default set-titles-string must expand #S:#I:#W, got {titles:?}"
    );
    assert!(
        titles[0].contains('"'),
        "default set-titles-string quotes #T, got {titles:?}"
    );
}

/// `set-titles off` is tmux's gate for touching the outer terminal at all:
/// measured on tmux 3.7b, an attached client emits neither OSC 0 nor OSC 7.
#[tokio::test]
async fn set_titles_off_writes_no_title_and_no_path() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    // Turn the osc7 capability on so the path would be emitted if it were not
    // gated, then drive a refresh with set-titles left at its "off" default.
    let set = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::TerminalFeatures,
            value: "xterm*:osc7".to_owned(),
            mode: SetOptionMode::Append,
        }))
        .await;
    assert!(matches!(set, Response::SetOption(_)));

    let target = recv_switch_target(&mut control_rx, "terminal-features refresh").await;
    assert!(
        target.outer_terminal.features_string().contains("osc7"),
        "fixture must advertise osc7"
    );
    let frame = String::from_utf8_lossy(&target.render_frame).into_owned();
    assert!(
        !frame.contains(TITLE_OPEN),
        "set-titles off must not write a title, got {frame:?}"
    );
    assert!(
        !frame.contains("\u{1b}]7;"),
        "set-titles off must not write OSC 7 either, got {frame:?}"
    );
    assert!(
        target.client_title.is_none(),
        "no title is resolved while set-titles is off"
    );
}

/// tmux compares each expansion against the client's remembered `c->title`, so
/// a refresh that resolves the same title stays silent. Without this the title
/// would be rewritten on every status tick.
#[tokio::test]
async fn an_unchanged_title_is_not_re_emitted_on_the_next_refresh() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(&handler, OptionName::SetTitlesString, "STATIC-TITLE").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;
    let first = recv_switch_target(&mut control_rx, "first refresh").await;
    assert_eq!(
        titles_in(&first.render_frame),
        vec!["STATIC-TITLE".to_owned()]
    );

    // Any later refresh re-renders the prelude; the title must not repeat.
    set_global(&handler, OptionName::StatusInterval, "7").await;
    let second = recv_switch_target(&mut control_rx, "second refresh").await;
    assert!(
        titles_in(&second.render_frame).is_empty(),
        "an unchanged title must not be re-emitted, got {:?}",
        titles_in(&second.render_frame)
    );
    assert_eq!(client_title_of(&second), Some("STATIC-TITLE"));

    // A genuinely different expansion is emitted again.
    set_global(&handler, OptionName::SetTitlesString, "OTHER-TITLE").await;
    let third = recv_switch_target(&mut control_rx, "third refresh").await;
    assert_eq!(
        titles_in(&third.render_frame),
        vec!["OTHER-TITLE".to_owned()]
    );
}

/// A client whose terminal never advertised TSL/FSL keeps its title untouched,
/// exactly as tmux's `tty_set_title()` returns early without both capabilities.
#[tokio::test]
async fn a_client_without_the_title_capability_receives_no_title() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            // No TERM: the Windows Terminal case from issue #182.
            OuterTerminalContext::default(),
        )
        .await;

    set_global(&handler, OptionName::SetTitlesString, "RMUXTEST #S").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    let target = recv_switch_target(&mut control_rx, "set-titles on refresh").await;
    assert!(
        !target.outer_terminal.features_string().contains("title"),
        "fixture must not advertise the title capability"
    );
    let frame = String::from_utf8_lossy(&target.render_frame).into_owned();
    assert!(
        !frame.contains(TITLE_OPEN) && !frame.contains("RMUXTEST"),
        "a title-incapable client must receive no title, got {frame:?}"
    );
}

/// Issue #182, step 3 of the report: a client with no `TERM` (Windows
/// Terminal) gains the title capability from `terminal-features '*:title'`,
/// and once it re-attaches `set-titles on` must reach its outer terminal.
#[tokio::test]
async fn issue_182_wildcard_title_feature_gives_a_term_less_client_its_title() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;
    set_global(&handler, OptionName::SetTitlesString, "RMUXTEST #S").await;
    append_global(&handler, OptionName::TerminalFeatures, "*:title").await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            OuterTerminalContext::default(),
        )
        .await;
    drain_attach_controls(&mut control_rx);
    set_global(&handler, OptionName::SetTitles, "on").await;

    let target = recv_switch_target(&mut control_rx, "set-titles on refresh").await;
    assert!(
        target.outer_terminal.features_string().contains("title"),
        "'*:title' must advertise the title capability to a TERM-less client"
    );
    assert_eq!(
        titles_in(&target.render_frame),
        vec!["RMUXTEST alpha".to_owned()],
        "the re-attached TERM-less client must receive the expanded title"
    );
}

/// Control characters inside the expansion must not be able to close the OSC
/// and inject a second sequence. tmux 3.7b writes the expansion verbatim and
/// is injectable; RMUX neutralises the payload instead.
#[tokio::test]
async fn control_characters_in_the_title_cannot_inject_a_second_sequence() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(
        &handler,
        OptionName::SetTitlesString,
        "A\u{1b}]0;INJECT\u{7}B\tC",
    )
    .await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    let target = recv_switch_target(&mut control_rx, "set-titles on refresh").await;
    let frame = String::from_utf8_lossy(&target.render_frame).into_owned();
    let titles = titles_in(&target.render_frame);
    assert_eq!(titles.len(), 1, "exactly one OSC 0, got {titles:?}");
    assert!(
        !titles[0].contains('\u{1b}') && !titles[0].contains('\u{7}'),
        "no control character may survive into the title, got {titles:?}"
    );
    assert_eq!(
        frame.matches(TITLE_OPEN).count(),
        1,
        "the payload must not open a second OSC 0, got {frame:?}"
    );
}

/// The reporter's scenario: a program running in the *active* pane retitles
/// itself with OSC 2. That pane's output never triggered a client refresh, so
/// the expanded title stayed stale on the outer terminal. Only a re-render
/// carries the new title, and the refresh must be driven by the title change.
#[tokio::test]
async fn an_active_pane_title_change_carries_the_new_title_to_the_client() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_quiet_session(&handler, &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(&handler, OptionName::SetTitlesString, "T:#{pane_title}").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles on refresh").await;

    // The application in the active pane emits OSC 2.
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"\x1b]2;APP-DRIVEN\x07",
    )
    .await;
    handler
        .pane_alert_callback()
        .alert(crate::pane_io::PaneAlertEvent {
            session_name: alpha,
            pane_id,
            bell_count: 0,
            title_changed: true,
            title_change: None,
            path_changed: false,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            clipboard_queries: Vec::new(),
            mouse_mode_changed: false,
            alternate_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        });

    let refreshed = recv_switch_target(&mut control_rx, "pane title change refresh").await;
    assert_eq!(
        titles_in(&refreshed.render_frame),
        vec!["T:APP-DRIVEN".to_owned()],
        "an active pane retitling itself must reach the outer terminal"
    );
}

/// The same event with `set-titles off` must not cost a redraw: nothing would
/// be written to the outer terminal, so no client refresh is warranted.
#[tokio::test]
async fn an_active_pane_title_change_does_not_refresh_when_set_titles_is_off() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_quiet_session(&handler, &alpha).await;
    let target = PaneTarget::with_window(alpha.clone(), 0, 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0).map(|pane| pane.id()))
            .expect("window pane exists")
    };

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"\x1b]2;APP-DRIVEN\x07",
    )
    .await;
    handler
        .pane_alert_callback()
        .alert(crate::pane_io::PaneAlertEvent {
            session_name: alpha,
            pane_id,
            bell_count: 0,
            title_changed: true,
            title_change: None,
            path_changed: false,
            clipboard_set: false,
            clipboard_writes: Vec::new(),
            clipboard_queries: Vec::new(),
            mouse_mode_changed: false,
            alternate_mode_changed: false,
            queue_activity_alert: false,
            generation: None,
        });

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(750), control_rx.recv())
            .await
            .is_err(),
        "set-titles off must not turn a pane title change into a client redraw"
    );
}

/// Turning `set-titles` back on must not rewrite a title the terminal already
/// shows: tmux keeps `c->title` across the option going off and on again.
#[tokio::test]
async fn toggling_set_titles_off_and_on_does_not_rewrite_an_unchanged_title() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(&handler, OptionName::SetTitlesString, "STABLE").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;
    let first = recv_switch_target(&mut control_rx, "titles on").await;
    assert_eq!(titles_in(&first.render_frame), vec!["STABLE".to_owned()]);

    set_global(&handler, OptionName::SetTitles, "off").await;
    let off = recv_switch_target(&mut control_rx, "titles off").await;
    assert!(titles_in(&off.render_frame).is_empty());
    assert!(off.client_title.is_none());

    set_global(&handler, OptionName::SetTitles, "on").await;
    let back_on = recv_switch_target(&mut control_rx, "titles on again").await;
    assert!(
        titles_in(&back_on.render_frame).is_empty(),
        "a title the terminal already shows must not be rewritten, got {:?}",
        titles_in(&back_on.render_frame)
    );
}

/// A client that is not draining lets the control queue replace one pending
/// render refresh with the next. The frame carrying the title must survive
/// that: its successor deduplicated the title away, so dropping it would leave
/// the outer terminal on the old title with nothing left to correct it.
#[tokio::test]
async fn a_title_change_survives_a_coalesced_follow_up_refresh() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(&handler, OptionName::SetTitlesString, "FIRST").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;
    let first = recv_switch_target(&mut control_rx, "set-titles on refresh").await;
    assert_eq!(titles_in(&first.render_frame), vec!["FIRST".to_owned()]);

    // Queue the title change and an unrelated redraw back to back, draining
    // nothing in between so the second refresh can replace the first.
    set_global(&handler, OptionName::SetTitlesString, "SECOND").await;
    set_global(&handler, OptionName::StatusInterval, "11").await;

    assert_eq!(
        drain_queued_titles(&mut control_rx),
        vec!["SECOND".to_owned()],
        "the changed title must reach the client exactly once"
    );
}

/// Every OSC 0 payload still queued for this client, oldest first.
fn drain_queued_titles(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) -> Vec<String> {
    let mut titles = Vec::new();
    while let Ok(control) = control_rx.try_recv() {
        if matches!(control, AttachControl::Switch(_)) {
            titles.extend(titles_in(&take_switch_target(control).render_frame));
        } else if let AttachControl::Write(bytes) = control {
            titles.extend(titles_in(&bytes));
        }
    }
    titles
}

/// The listener seeds the fresh client's remembered title from the very frame
/// it is about to forward, so the client's first refresh must stay silent.
/// Without that seam the same title is written twice per attach.
#[tokio::test]
async fn the_attach_frame_title_is_not_repeated_by_the_first_refresh() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;
    set_global(&handler, OptionName::SetTitlesString, "SEEDED-TITLE").await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    // Exactly what handle_attach_session builds and listener.rs forwards.
    let target = {
        let state = handler.state.lock().await;
        crate::handler::attach_support::attach_target_for_session(
            &state,
            &alpha,
            crate::handler::attach_support::AttachSessionRenderOptions {
                attached_count: 1,
                terminal_context: &title_capable_context(),
                previous_title: None,
                client: None,
                socket_path: Path::new("rmux.sock"),
                client_loop_snapshots: None,
            },
        )
        .expect("attach target builds")
    };
    assert_eq!(
        titles_in(&target.render_frame),
        vec!["SEEDED-TITLE".to_owned()],
        "the attach frame itself carries the first title"
    );

    // The listener's own seam, not a copy of it: removing the seed makes this
    // client's first refresh repeat the title and this test red.
    let seeded = attach_with_initial_title(
        &handler,
        &alpha,
        crate::listener::attach_frame_client_title(&target),
    )
    .await;
    let unseeded = attach_with_initial_title(&handler, &alpha, None).await;

    // Any refresh re-renders the prelude for both clients.
    set_global(&handler, OptionName::StatusInterval, "9").await;

    let (mut seeded_rx, mut unseeded_rx) = (seeded, unseeded);
    let seeded_refresh = recv_switch_target(&mut seeded_rx, "seeded refresh").await;
    assert!(
        titles_in(&seeded_refresh.render_frame).is_empty(),
        "a seeded client must not be told its title twice, got {:?}",
        titles_in(&seeded_refresh.render_frame)
    );

    let unseeded_refresh = recv_switch_target(&mut unseeded_rx, "unseeded refresh").await;
    assert_eq!(
        titles_in(&unseeded_refresh.render_frame),
        vec!["SEEDED-TITLE".to_owned()],
        "without the registration seam the first refresh repeats the title"
    );
}

/// Registers a client exactly as `listener.rs` does, with the title the attach
/// frame already delivered.
async fn attach_with_initial_title(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
    client_title: Option<crate::outer_terminal::ClientTitleState>,
) -> mpsc::UnboundedReceiver<AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let uid = current_owner_uid();
    let attach_pid = if client_title.is_some() { 4182 } else { 4183 };
    handler
        .register_attach_with_access(
            attach_pid,
            session.clone(),
            None,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: title_capable_context(),
                client_title,
                flags: crate::handler::attach_support::ClientFlags::default(),
                render_stream: false,
                uid,
                user: rmux_os::identity::UserIdentity::Uid(uid),
                can_write: true,
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
        )
        .await
        .expect("attach registration succeeds");
    control_rx
}

/// The web and snapshot render paths share `render_prelude`, but a browser
/// client advertises no terminal capabilities at all, so `set-titles on` must
/// not push OSC 0 / OSC 7 into a snapshot that xterm.js will replay.
#[cfg(feature = "web")]
#[tokio::test]
async fn the_web_and_snapshot_renders_carry_no_title() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;
    set_global(&handler, OptionName::SetTitlesString, "WEB-MUST-NOT-APPEAR").await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    let state = handler.state.lock().await;
    // Exactly the two entry points handler_web.rs uses.
    let web_attach = crate::handler::attach_support::attach_target_for_web_session(
        &state,
        &alpha,
        1,
        &OuterTerminalContext::default(),
        Path::new("rmux.sock"),
    )
    .expect("web attach target builds");
    let snapshot = crate::handler::attach_support::attach_render_target_for_session_window(
        &state,
        &alpha,
        Some(0),
        1,
        &OuterTerminalContext::default(),
        Path::new("rmux.sock"),
    )
    .expect("web window render builds");

    for (label, frame) in [
        ("web attach", &web_attach.render_frame),
        ("web window snapshot", &snapshot.render_frame),
    ] {
        let text = String::from_utf8_lossy(frame).into_owned();
        assert!(
            titles_in(frame).is_empty() && !text.contains("WEB-MUST-NOT-APPEAR"),
            "{label} must carry no title, got {text:?}"
        );
        assert!(
            !text.contains("\u{1b}]7;"),
            "{label} must carry no OSC 7, got {text:?}"
        );
    }
}

/// tmux 3.7b runs `#(shell-command)` inside `set-titles-string`: the first
/// expansion is empty and a later one carries the job's stdout, re-run no more
/// often than `status-interval`. rmux shares the daemon's status-job runtime,
/// so the title behaves the same instead of silently dropping the job.
#[tokio::test]
async fn a_shell_command_in_the_title_expands_through_the_status_job_runtime() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            std::process::id(),
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    let marker = format!("TITLEJOB{}", std::process::id());
    set_global(
        &handler,
        OptionName::SetTitlesString,
        &format!("J:#(echo {marker})"),
    )
    .await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;

    // The job is asynchronous, exactly as in tmux: the first expansion is empty.
    let first = recv_switch_target(&mut control_rx, "set-titles on refresh").await;
    assert_eq!(
        titles_in(&first.render_frame),
        vec!["J:".to_owned()],
        "a job's first expansion is empty, never the literal #() text"
    );

    // A later render picks the completed output out of the status-job cache.
    let deadline = std::time::Instant::now() + ATTACH_LIFECYCLE_TIMEOUT;
    loop {
        set_global(&handler, OptionName::StatusInterval, "1").await;
        let refreshed = recv_switch_target(&mut control_rx, "job output refresh").await;
        if let Some(title) = titles_in(&refreshed.render_frame).first() {
            assert_eq!(title, &format!("J:{marker}"));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "status job output never reached the title"
        );
        sleep(Duration::from_millis(25)).await;
    }
    handler.shutdown_status_jobs().await;
}

/// The periodic status tick is rmux's equivalent of tmux re-expanding the
/// title on every server loop: a title whose expansion drifted (a job, a
/// strftime token, `#{session_alerts}`) must reach the client without waiting
/// for an unrelated redraw, and an unchanged one must stay silent.
#[tokio::test]
async fn the_status_tick_carries_a_changed_title_and_repeats_nothing() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    new_detached_session(&handler, &alpha).await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let attach_pid = std::process::id();
    let attach_id = handler
        .register_attach_with_terminal_context(
            attach_pid,
            alpha.clone(),
            control_tx,
            title_capable_context(),
        )
        .await;

    set_global(&handler, OptionName::SetTitlesString, "TICK-ONE").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles-string refresh").await;
    set_global(&handler, OptionName::SetTitles, "on").await;
    let _ = recv_switch_target(&mut control_rx, "set-titles on refresh").await;

    // A tick with the title unchanged writes the status frame and no title.
    handler
        .refresh_attached_client_status_for_identity(attach_pid, attach_id, &alpha)
        .await
        .expect("status refresh succeeds");
    let quiet = recv_status_write(&mut control_rx, "unchanged status tick").await;
    assert!(
        titles_in(&quiet).is_empty(),
        "an unchanged title must not be rewritten by the tick, got {:?}",
        titles_in(&quiet)
    );

    // Change the expansion without touching any client, then tick again.
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::SetTitlesString,
                "TICK-TWO".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set-titles-string set succeeds");
    }
    handler
        .refresh_attached_client_status_for_identity(attach_pid, attach_id, &alpha)
        .await
        .expect("status refresh succeeds");
    let updated = recv_status_write(&mut control_rx, "changed status tick").await;
    assert_eq!(
        titles_in(&updated),
        vec!["TICK-TWO".to_owned()],
        "the tick must carry a drifted title to the outer terminal"
    );

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("attach is active");
    assert_eq!(
        active.client_title.title(),
        Some("TICK-TWO"),
        "the tick remembers what it just wrote"
    );
}

async fn recv_status_write(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    context: &str,
) -> Vec<u8> {
    match recv_matching_attach_control(control_rx, context, |control| {
        matches!(control, AttachControl::Write(_))
    })
    .await
    {
        AttachControl::Write(bytes) => bytes,
        other => panic!("expected a status write for {context}, got {other:?}"),
    }
}
