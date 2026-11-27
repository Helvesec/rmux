//! `switch-client -r` and `attach-session -f <control flags>` on a control
//! client, against tmux 3.7b (measured 2026-11-18 with the recipe
//! `sw.sh`): `switch-client -r` toggles `read-only` and `ignore-size`
//! together, recalculates the window size without (or with) the client and
//! sends it `%session-changed`; `attach-session -f` takes `no-output`,
//! `wait-exit` and `pause-after[=N]`, and `!flag` clears them.

use super::read_only_tests::{assert_read_only_refusal, control_flags, run_control_command};
use super::*;
use crate::handler::control_support::ControlClientIdentity;

const CONTROL_PREFIX: &str = "attached,focused,control-mode";

/// `#{client_flags}` between `control-mode` and the terminal's `UTF-8`.
async fn own_flags(handler: &RequestHandler, requester_pid: u32) -> String {
    let flags = control_flags(handler, requester_pid).await;
    flags
        .strip_prefix(CONTROL_PREFIX)
        .unwrap_or_else(|| panic!("a control client: {flags}"))
        .trim_end_matches("UTF-8")
        .trim_matches(',')
        .to_owned()
}

async fn attached_control_client(
    handler: &RequestHandler,
    session: &SessionName,
    requester_pid: u32,
) -> (ControlClientIdentity, mpsc::Receiver<ControlServerEvent>) {
    create_session(handler, session.clone(), INITIAL_SIZE).await;
    let (control_id, events) =
        register_control_client_with_id(handler, requester_pid, session).await;
    (
        ControlClientIdentity::new(requester_pid, control_id),
        events,
    )
}

async fn assert_session_changed(
    events: &mut mpsc::Receiver<ControlServerEvent>,
    session: &SessionName,
) {
    let lines = collect_control_notifications_through(events, "%session-changed ").await;
    let line = lines
        .last()
        .filter(|line| line.starts_with("%session-changed $"))
        .unwrap_or_else(|| panic!("tmux 3.7b announces the re-committed session: {lines:?}"));
    assert!(line.ends_with(&format!(" {session}")), "{line}");
}

#[tokio::test]
async fn switch_client_r_toggles_read_only_and_ignore_size_together_like_tmux() {
    // tmux 3.7b, `-C attach <flags>` then `switch-client -r` twice: the flags
    // after each toggle, whatever the client started with.
    for (index, (attach, first, second)) in [
        ("", "ignore-size,read-only", ""),
        ("-r", "", "ignore-size,read-only"),
        ("-f read-only", "", "ignore-size,read-only"),
        ("-f ignore-size", "ignore-size,read-only", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-switch-r-{index}"));
        let requester_pid = std::process::id();
        let (identity, mut events) =
            attached_control_client(&handler, &session, requester_pid).await;
        run_control_command(
            &handler,
            identity,
            &format!("attach-session {attach} -t {session}"),
        )
        .await
        .expect("control attach");
        settle_control_notifications(&mut events).await;

        for expected in [first, second] {
            run_control_command(&handler, identity, "switch-client -r")
                .await
                .expect("tmux 3.7b accepts switch-client -r from a control client");
            assert_eq!(
                own_flags(&handler, requester_pid).await,
                expected,
                "{attach}"
            );
            assert_session_changed(&mut events, &session).await;
            let sent =
                run_control_command(&handler, identity, &format!("send-keys -t {session} -l X"))
                    .await;
            if expected.contains("read-only") {
                assert_read_only_refusal(sent);
            } else {
                sent.expect("a writable control client sends keys");
            }
        }
    }
}

#[tokio::test]
async fn switch_client_r_with_a_target_toggles_then_switches_like_tmux() {
    let handler = RequestHandler::new();
    let source = session_name("control-switch-r-source");
    let target = session_name("control-switch-r-target");
    let requester_pid = std::process::id();
    let (identity, mut events) = attached_control_client(&handler, &source, requester_pid).await;
    create_session(&handler, target.clone(), INITIAL_SIZE).await;
    settle_control_notifications(&mut events).await;

    run_control_command(&handler, identity, &format!("switch-client -r -t {target}"))
        .await
        .expect("switch-client -r -t");
    assert_session_changed(&mut events, &target).await;
    assert_eq!(
        own_flags(&handler, requester_pid).await,
        "ignore-size,read-only"
    );
    let active_control = handler.active_control.lock().await;
    let active = active_control
        .by_pid
        .get(&requester_pid)
        .expect("control client remains registered");
    assert_eq!(active.session_name.as_ref(), Some(&target));
}

#[tokio::test]
async fn switch_client_r_recalculates_the_window_size_without_the_client_like_tmux() {
    // tmux 3.7b, two control clients: the witness declares its size first,
    // then the client declares its own and toggles read-only twice.
    //   largest:  witness 60x20,  client 120x40 -> 120x40, 60x20, 120x40
    //   smallest: witness 120x40, client 60x20  -> 60x20, 120x40, 60x20
    // The witness sees `%client-session-changed` before each `%layout-change`.
    let small = TerminalSize { cols: 60, rows: 20 };
    let large = TerminalSize {
        cols: 120,
        rows: 40,
    };
    for (policy, witness_size, client_size) in
        [("largest", small, large), ("smallest", large, small)]
    {
        let handler = RequestHandler::new();
        let session = session_name(&format!("control-switch-r-{policy}"));
        let requester_pid = std::process::id();
        let witness_pid = requester_pid.saturating_add(1);
        let (identity, mut events) =
            attached_control_client(&handler, &session, requester_pid).await;
        set_window_size_policy(&handler, &session, policy).await;
        let (_witness_id, mut witness_events) =
            register_control_client_with_id(&handler, witness_pid, &session).await;
        for (pid, size) in [(witness_pid, witness_size), (requester_pid, client_size)] {
            let response = handler
                .handle(Request::RefreshClient(Box::new(
                    refresh_client_size_request(pid, size),
                )))
                .await;
            assert!(
                matches!(response, Response::RefreshClient(_)),
                "{response:?}"
            );
        }
        assert_eq!(
            session_size(&handler, &session).await,
            client_size,
            "{policy}"
        );
        let layout_prefix = format!(
            "%layout-change @{} ",
            active_window_id(&handler, &session).await
        );
        settle_control_notifications(&mut events).await;
        settle_control_notifications(&mut witness_events).await;

        for expected in [witness_size, client_size] {
            run_control_command(&handler, identity, "switch-client -r")
                .await
                .expect("switch-client -r");
            assert_eq!(session_size(&handler, &session).await, expected, "{policy}");
            let lines =
                collect_control_notifications_through(&mut witness_events, &layout_prefix).await;
            let layout = lines
                .iter()
                .position(|line| line.starts_with(&layout_prefix))
                .unwrap_or_else(|| panic!("{policy}: the witness sees the resize: {lines:?}"));
            let expected_geometry = format!("{}x{}", expected.cols, expected.rows);
            assert_eq!(
                layout_change_geometry(&lines[layout]).as_deref(),
                Some(expected_geometry.as_str()),
                "{policy}: {lines:?}"
            );
            assert!(
                lines[..layout]
                    .iter()
                    .any(|line| line.starts_with("%client-session-changed ")),
                "{policy}: tmux 3.7b reports the re-committed client first: {lines:?}"
            );
            assert_session_changed(&mut events, &session).await;
        }
    }
}

#[tokio::test]
async fn control_attach_flags_take_the_control_only_flags_like_tmux() {
    // tmux 3.7b `-C attach -f no-output,pause-after=3,wait-exit,ignore-size`
    // reports `ignore-size,no-output,wait-exit,pause-after=3`; attaching again
    // with `-f '!no-output,!wait-exit,pause-after=1'` leaves `pause-after=1`.
    let handler = RequestHandler::new();
    let session = session_name("control-attach-flags");
    let requester_pid = std::process::id();
    let (identity, _events) = attached_control_client(&handler, &session, requester_pid).await;

    for (flags, expected) in [
        (
            "no-output,pause-after=3,wait-exit,ignore-size",
            "ignore-size,no-output,wait-exit,pause-after=3",
        ),
        (
            "!no-output,!wait-exit,pause-after=1",
            "ignore-size,pause-after=1",
        ),
        ("!ignore-size,pause-after", "pause-after=0"),
        ("!pause-after,no-output,read-only", "no-output,read-only"),
    ] {
        run_control_command(
            &handler,
            identity,
            &format!("attach-session -f '{flags}' -t {session}"),
        )
        .await
        .unwrap_or_else(|error| panic!("tmux 3.7b accepts -f {flags}: {error}"));
        assert_eq!(
            own_flags(&handler, requester_pid).await,
            expected,
            "-f {flags}"
        );
    }
}

#[tokio::test]
async fn control_attach_flags_still_refuse_unsupported_and_unknown_flags() {
    let handler = RequestHandler::new();
    let session = session_name("control-attach-flags-refused");
    let requester_pid = std::process::id();
    let (identity, _events) = attached_control_client(&handler, &session, requester_pid).await;

    for (flags, message) in [
        (
            "no-output,active-pane",
            "attach-session client flags are not available for control clients",
        ),
        ("no-output,bogus", "unknown client flag: bogus"),
    ] {
        let error = run_control_command(
            &handler,
            identity,
            &format!("attach-session -f '{flags}' -t {session}"),
        )
        .await
        .expect_err("refused before any flag is applied");
        assert!(error.to_string().contains(message), "{flags}: {error}");
        assert_eq!(
            own_flags(&handler, requester_pid).await,
            "",
            "{flags}: a refused attach applies no flag"
        );
    }
}

async fn session_cwd(
    handler: &RequestHandler,
    session: &SessionName,
) -> Option<std::path::PathBuf> {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .expect("session remains present")
        .cwd()
        .map(std::path::Path::to_path_buf)
}

#[tokio::test]
async fn control_attach_refuses_unsupported_flags_before_any_attach_effect() {
    // R3451 probes X-ap-cwd and X-nd-cwd: a refused `attach -c <dir> -f
    // active-pane -t s2` changed the session directory of s2 first. The
    // refusal now comes with the flag parse, so `-c`, `-d`/`-x` and the
    // client environment are never reached.
    for (index, (other_clients, flag)) in [("-d", "active-pane"), ("-x", "no-detach-on-destroy")]
        .into_iter()
        .enumerate()
    {
        let handler = RequestHandler::new();
        let source = session_name(&format!("control-attach-refused-source-{index}"));
        let target = session_name(&format!("control-attach-refused-target-{index}"));
        let requester_pid = std::process::id();
        let other_pid = requester_pid.saturating_add(1);
        let (identity, _events) = attached_control_client(&handler, &source, requester_pid).await;
        create_session(&handler, target.clone(), INITIAL_SIZE).await;
        let (_other_id, mut other_controls) =
            register_attached_client(&handler, other_pid, &target, TARGET_ATTACHED_SIZE).await;
        let cwd_before = session_cwd(&handler, &target).await;
        let size_before = session_size(&handler, &target).await;

        let error = run_control_command(
            &handler,
            identity,
            &format!(
                "attach-session {other_clients} -c /refused-attach-directory -f {flag} -t {target}"
            ),
        )
        .await
        .expect_err("a control client refuses the flag");
        assert!(
            error
                .to_string()
                .contains("attach-session client flags are not available for control clients"),
            "{flag}: {error}"
        );

        assert_eq!(
            session_cwd(&handler, &target).await,
            cwd_before,
            "{flag}: a refused attach -c keeps the session directory"
        );
        assert_eq!(session_size(&handler, &target).await, size_before, "{flag}");
        while let Ok(control) = other_controls.try_recv() {
            assert!(
                !matches!(
                    control,
                    crate::pane_io::AttachControl::Detach
                        | crate::pane_io::AttachControl::DetachKill
                ),
                "{other_clients} {flag}: a refused attach detaches no other client"
            );
        }
        assert!(handler
            .active_attach
            .lock()
            .await
            .by_pid
            .contains_key(&other_pid));
        assert_eq!(own_flags(&handler, requester_pid).await, "", "{flag}");
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .expect("control client remains registered");
        assert_eq!(active.session_name.as_ref(), Some(&source), "{flag}");
    }
}

#[tokio::test]
async fn switch_client_r_sizes_the_target_like_a_client_already_ignoring_size() {
    // R3451 probe X-r-tsized: a sized control client toggles read-only and
    // ignore-size while it switches to a session no other client sizes. The
    // commit sizes that session from the toggled flags, so the result is the
    // one of `refresh-client -f ignore-size` then `switch-client -t`.
    // (tmux 3.7b gives the client size to both, as `ignore_client_size()`
    // only skips the client beside another unmarked one; that difference is
    // inherited and outside this test.)
    let mut sizes = Vec::new();
    for (index, commands) in [
        ["refresh-client -f ignore-size", "switch-client -t"],
        ["", "switch-client -r -t"],
    ]
    .into_iter()
    .enumerate()
    {
        let handler = RequestHandler::new();
        let source = session_name(&format!("control-switch-r-sized-source-{index}"));
        let target = session_name(&format!("control-switch-r-sized-target-{index}"));
        let requester_pid = std::process::id();
        let (identity, _events) = attached_control_client(&handler, &source, requester_pid).await;
        create_session(&handler, target.clone(), INITIAL_SIZE).await;
        let response = handler
            .handle(Request::RefreshClient(Box::new(
                refresh_client_size_request(requester_pid, CONTROL_SIZE),
            )))
            .await;
        assert!(
            matches!(response, Response::RefreshClient(_)),
            "{response:?}"
        );
        assert_eq!(session_size(&handler, &target).await, INITIAL_SIZE);
        let [before, switch] = commands;
        if !before.is_empty() {
            run_control_command(&handler, identity, before)
                .await
                .expect(before);
        }
        run_control_command(&handler, identity, &format!("{switch} {target}"))
            .await
            .expect(switch);
        assert!(
            own_flags(&handler, requester_pid)
                .await
                .contains("ignore-size"),
            "{switch}"
        );
        sizes.push(session_size(&handler, &target).await);
    }
    assert_eq!(sizes[0], sizes[1], "{sizes:?}");
}
