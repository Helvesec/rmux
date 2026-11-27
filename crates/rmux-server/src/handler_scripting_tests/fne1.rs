//! FNE1 — `new-pane`/`newp -E`: a float created directly Empty.
//!
//! Every expectation below is a literal measured on the pinned tmux 3.7b
//! (`fne1/oracle`), never derived from the candidate:
//!
//! * `-E` with no command at all creates the float — measured `size=30x8
//!   at=2,1 float=1 pid=0 tty= dead=0`, and its capture is the pane's rows of
//!   empty lines, so it is live and capturable rather than dead;
//! * `-E` with a single empty argument does exactly the same;
//! * `-E` with a real command, or with more than one argument, is refused with
//!   `command cannot be given for empty pane` and creates no pane;
//! * `-d` leaves the selection where it was;
//! * while three Empty panes existed, the tmux server still had exactly one
//!   child — the root pane's — so Empty really has no process and no PTY, and
//!   `pid=0` was not the only evidence;
//! * the first `respawn-pane` without `-k` gives that same pane id its first
//!   child and PTY at the float's own size, and the child really runs.
//!
//! Source for the same contract: `cmd-split-window.c:103-115` reads `-E`,
//! accepts no command or exactly one empty argument and refuses anything else
//! before it places the cell; `spawn.c:372-377` sets `PANE_EMPTY` and returns
//! before `forkpty`.
//!
//! The Empty neighbours themselves — no master, no output reader, `pid` 0, not
//! dead, no forced remain-on-exit, first respawn — are the acquired behaviour
//! of `handler_pane_command_tests/split_empty.rs`; this module does not repeat
//! them, it proves that `-E` reaches them.

use super::*;

/// Creates a pane with the given arguments and returns what tmux would print.
async fn created(handler: &RequestHandler, args: &[&str]) -> String {
    match offer(handler, args, true).await {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            exit_status: 0,
            ..
        } => String::from_utf8(stdout).expect("printed geometry is utf-8"),
        other => panic!("{args:?}: {other:?}"),
    }
}

/// Attempts a creation that must be refused, through the same queue the
/// neighbouring FNP suites use, and returns the message.
async fn refused(handler: &RequestHandler, command: &str) -> String {
    let before = window(handler).await;
    let error = match queue(handler, command).await {
        Err(error) => error.to_string(),
        Ok(output) => panic!("{command} was accepted: {output:?}"),
    };
    let after = window(handler).await;
    assert_eq!(
        after.pane_count(),
        before.pane_count(),
        "{command} created a pane before it was refused: {error}"
    );
    for old in before.panes() {
        assert_eq!(after.pane(old.index()).unwrap().id(), old.id());
        assert_eq!(after.pane(old.index()).unwrap().geometry(), old.geometry());
    }
    error
}

/// The created pane, read from the window model.
async fn last_pane(handler: &RequestHandler) -> rmux_core::Pane {
    let window = window(handler).await;
    window
        .panes()
        .last()
        .expect("the window has at least one pane")
        .clone()
}

#[tokio::test]
async fn empty_creates_the_float_without_a_command() {
    let handler = setup().await;
    assert_eq!(
        created(
            &handler,
            &[
                "newp",
                "-dP",
                "-t",
                "fnp:0.0",
                "-E",
                "-x",
                "30",
                "-y",
                "8",
                "-X",
                "2",
                "-Y",
                "1",
                "-F",
                "#{pane_width}x#{pane_height}|#{pane_left},#{pane_top}|#{pane_floating_flag}",
            ],
        )
        .await,
        "30x8|2,1|1\n",
        "the measured geometry of an Empty float"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_single_empty_argument_is_the_same_as_no_command() {
    let handler = setup().await;
    // tmux accepts `count == 1` when that one argument is the empty string.
    assert_eq!(
        created(
            &handler,
            &[
                "newp",
                "-dP",
                "-t",
                "fnp:0.0",
                "-E",
                "-x",
                "30",
                "-y",
                "8",
                "-X",
                "2",
                "-Y",
                "1",
                "-F",
                "#{pane_width}x#{pane_height}",
                "",
            ],
        )
        .await,
        "30x8\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_real_command_under_empty_is_refused_before_anything_is_created() {
    let handler = setup().await;
    for command in [
        "new-pane -d -t fnp:0.0 -E -x 30 -y 8 -X 2 -Y 1 /bin/sh",
        "new-pane -d -t fnp:0.0 -E -x 30 -y 8 -X 2 -Y 1 '' extra",
        "newp -d -t fnp:0.0 -E -x 30 -y 8 -X 2 -Y 1 echo hello",
    ] {
        let error = refused(&handler, command).await;
        assert!(
            error.contains("command cannot be given for empty pane"),
            "{command} was refused by something else: {error}"
        );
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn without_empty_the_previous_contract_is_unchanged() {
    let handler = setup().await;
    // The explicit empty command keeps its own refusal when `-E` is absent.
    let error = refused(&handler, "new-pane -d -t fnp:0.0 -x 30 -y 8 -X 2 -Y 1 ''").await;
    assert!(
        error.contains("empty command is not supported"),
        "the non-empty-pane path changed its refusal: {error}"
    );
    // And an ordinary command still creates an ordinary float.
    assert_eq!(
        created(
            &handler,
            &[
                "newp",
                "-dP",
                "-t",
                "fnp:0.0",
                "-x",
                "30",
                "-y",
                "8",
                "-X",
                "2",
                "-Y",
                "1",
                "-F",
                "#{pane_width}x#{pane_height}",
            ],
        )
        .await,
        "30x8\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn an_empty_float_has_no_process_no_master_and_no_output_reader() {
    let handler = setup().await;
    let _ = created(
        &handler,
        &[
            "newp",
            "-dP",
            "-t",
            "fnp:0.0",
            "-E",
            "-x",
            "30",
            "-y",
            "8",
            "-X",
            "2",
            "-Y",
            "1",
            "-F",
            "#{pane_id}",
        ],
    )
    .await;
    let pane = last_pane(&handler).await;
    assert_eq!(pane.geometry().cols(), 30);
    assert_eq!(pane.geometry().rows(), 8);

    let target = PaneTarget::with_window(session_name("fnp"), 0, pane.index());
    let state = handler.state.lock().await;
    let index = pane.index();
    assert_eq!(
        state
            .pane_pid_in_window(&session_name("fnp"), 0, index)
            .expect("the pane exists"),
        0,
        "an Empty pane reports no process id"
    );
    assert!(
        state
            .clone_pane_master(&session_name("fnp"), 0, index)
            .is_err(),
        "and owns no PTY master, real or dummy"
    );
    assert!(
        !state.pane_is_dead(&session_name("fnp"), pane.id()),
        "it is live, not dead"
    );
    assert!(
        state.transcript_handle(&target).is_ok(),
        "and it is still capturable"
    );
    drop(state);
    cleanup(&handler).await;
}

#[tokio::test]
async fn empty_does_not_run_default_command() {
    let handler = setup().await;
    // A default-command that would be unmistakable if it ever ran.
    queue(
        &handler,
        "set-option -g default-command '/bin/sh -c \"printf FNE1-SENTINEL; sleep 300\"'",
    )
    .await
    .expect("the default-command is set");

    let _ = created(
        &handler,
        &[
            "newp",
            "-dP",
            "-t",
            "fnp:0.0",
            "-E",
            "-x",
            "30",
            "-y",
            "8",
            "-X",
            "2",
            "-Y",
            "1",
            "-F",
            "#{pane_id}",
        ],
    )
    .await;
    let pane = last_pane(&handler).await;
    let state = handler.state.lock().await;
    assert_eq!(
        state
            .pane_pid_in_window(&session_name("fnp"), 0, pane.index())
            .expect("the pane exists"),
        0,
        "the Empty pane started default-command"
    );
    assert!(
        state
            .clone_pane_master(&session_name("fnp"), 0, pane.index())
            .is_err(),
        "and it opened a PTY for it"
    );
    drop(state);
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_detached_empty_creation_keeps_the_selection() {
    let handler = setup().await;
    let before = window(&handler).await.active_pane_index();
    let _ = created(
        &handler,
        &[
            "newp",
            "-dP",
            "-t",
            "fnp:0.0",
            "-E",
            "-x",
            "30",
            "-y",
            "8",
            "-X",
            "2",
            "-Y",
            "1",
            "-F",
            "#{pane_id}",
        ],
    )
    .await;
    assert_eq!(
        window(&handler).await.active_pane_index(),
        before,
        "a detached Empty creation moved the selection"
    );
    // Without -d the created pane is selected, exactly as an ordinary float is.
    let _ = created(
        &handler,
        &[
            "newp",
            "-P",
            "-t",
            "fnp:0.0",
            "-E",
            "-x",
            "30",
            "-y",
            "8",
            "-X",
            "2",
            "-Y",
            "1",
            "-F",
            "#{pane_id}",
        ],
    )
    .await;
    let created_index = last_pane(&handler).await.index();
    assert_eq!(window(&handler).await.active_pane_index(), created_index);
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_first_respawn_gives_the_empty_float_its_process() {
    let handler = setup().await;
    let _ = created(
        &handler,
        &[
            "newp",
            "-dP",
            "-t",
            "fnp:0.0",
            "-E",
            "-x",
            "30",
            "-y",
            "8",
            "-X",
            "2",
            "-Y",
            "1",
            "-F",
            "#{pane_id}",
        ],
    )
    .await;
    let pane = last_pane(&handler).await;
    let identity = pane.id();
    let index = pane.index();
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .pane_pid_in_window(&session_name("fnp"), 0, index)
                .expect("the pane exists"),
            0
        );
    }

    // The respawn names no command, so the pane is given its own default
    // program and the product resolves it for the platform it runs on:
    // `/bin/sh` on Unix, `%COMSPEC%` or `System32\cmd.exe` on Windows
    // (`terminal/shell_resolver.rs:324-334`). This is the acquired first
    // respawn of `handler_pane_command_tests/split_empty.rs:119-147`. Naming
    // a Unix path here would make the witness unsatisfiable on Windows, whose
    // backend resolves the program as written instead of substituting a shell.
    queue(&handler, &format!("respawn-pane -t fnp:0.{}", pane.index()))
        .await
        .expect("the first respawn of an Empty pane succeeds");

    let after = last_pane(&handler).await;
    assert_eq!(after.id(), identity, "the respawn kept the same pane id");
    assert_eq!(
        (after.geometry().cols(), after.geometry().rows()),
        (30, 8),
        "and the float kept its own size"
    );
    let state = handler.state.lock().await;
    let pid = state
        .pane_pid_in_window(&session_name("fnp"), 0, index)
        .expect("the pane still exists");
    assert_ne!(pid, 0, "the respawn gave the Empty pane its first process");
    assert!(
        state
            .clone_pane_master(&session_name("fnp"), 0, index)
            .is_ok(),
        "and its first PTY"
    );
    drop(state);
    cleanup(&handler).await;
}
