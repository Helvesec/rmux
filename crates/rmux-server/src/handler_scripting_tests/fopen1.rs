//! FOPEN1 — opening, using and closing floats from the pane one is on.
//!
//! The expectations are literals measured on the pinned tmux 3.7b before any
//! line of this lot (`fopen1/native/oracle-fopen1.json`), in one
//! fixture whose window is 80x24.

use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;
use std::time::Duration;

/// Creates a float and returns what tmux prints for it.
async fn created(handler: &RequestHandler, args: &[&str]) -> String {
    let mut command = vec!["newp", "-dP"];
    command.extend_from_slice(args);
    command.extend_from_slice(&[
        "-F",
        "#{pane_left},#{pane_top} #{pane_width}x#{pane_height}",
    ]);
    match offer(handler, &command, true).await {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            exit_status: 0,
            ..
        } => String::from_utf8(stdout).expect("printed geometry is utf-8"),
        other => panic!("{args:?}: {other:?}"),
    }
}

#[tokio::test]
async fn an_omitted_target_opens_the_float_on_the_pane_one_is_on() {
    let handler = setup().await;
    // The session has one window and one pane, so the current pane is that one;
    // the float lands in its window without `-t` being written anywhere.
    let before = window(&handler).await;
    assert_eq!(before.pane_count(), 1);

    assert_eq!(
        created(&handler, &["-X", "4", "-Y", "2"]).await,
        "4,2 40x6\n"
    );

    let after = window(&handler).await;
    assert_eq!(
        after.pane_count(),
        2,
        "the float was created in the window of the current pane"
    );
    // The explicit form still addresses the same window, unchanged.
    assert_eq!(
        created(&handler, &["-t", "fnp:0.0", "-X", "6", "-Y", "3"]).await,
        "6,3 40x6\n"
    );
    assert_eq!(window(&handler).await.pane_count(), 3);
    cleanup(&handler).await;
}

/// Attempts a creation that must be refused, and returns the message.
async fn refused(handler: &RequestHandler, args: &[&str]) -> String {
    let before = window(handler).await;
    let command = format!("new-pane -d {}", args.join(" "));
    let error = match queue(handler, &command).await {
        Err(error) => error.to_string(),
        Ok(output) => panic!("{command} was accepted: {output:?}"),
    };
    assert_eq!(
        window(handler).await.pane_count(),
        before.pane_count(),
        "{args:?} created a pane before it was refused: {error}"
    );
    error
}

/// Attempts a creation through the runtime entry, and returns the message.
async fn refused_by_the_cli(handler: &RequestHandler, args: &[&str]) -> String {
    let before = window(handler).await;
    let mut command = vec!["newp", "-d"];
    command.extend_from_slice(args);
    let error = match offer(handler, &command, true).await {
        RuntimeCommandExecutionResponse::Complete {
            error: Some(error), ..
        } => error.to_string(),
        other => panic!("{args:?} was accepted: {other:?}"),
    };
    assert_eq!(
        window(handler).await.pane_count(),
        before.pane_count(),
        "{args:?} created a pane before it was refused: {error}"
    );
    error
}

/// Resizes the fixture window, so the edge the cascade meets is a small one.
async fn resize(handler: &RequestHandler, cols: u16, rows: u16) {
    queue(
        handler,
        &format!("resize-window -t fnp:0 -x {cols} -y {rows}"),
    )
    .await
    .expect("the detached window resizes");
    let size = window(handler).await.size();
    assert_eq!((size.cols, size.rows), (cols, rows));
}

#[tokio::test]
async fn two_omitted_openings_cascade_on_both_axes() {
    let handler = setup().await;
    // Measured: the first float of an 80x24 window is 40x6 at (4,2) and the
    // second, with the first still open, at (8,4).
    assert_eq!(created(&handler, &[]).await, "4,2 40x6\n");
    assert_eq!(created(&handler, &[]).await, "8,4 40x6\n");
    assert_eq!(window(&handler).await.pane_count(), 3);
    cleanup(&handler).await;
}

#[tokio::test]
async fn an_explicit_axis_does_not_consume_the_other() {
    let handler = setup().await;
    assert_eq!(created(&handler, &[]).await, "4,2 40x6\n");
    assert_eq!(created(&handler, &[]).await, "8,4 40x6\n");
    // Measured: an explicit X is used as written and the omitted Y walks on,
    // while X's own memory stays where it was.
    assert_eq!(created(&handler, &["-X", "30"]).await, "30,6 40x6\n");
    assert_eq!(created(&handler, &[]).await, "12,8 40x6\n");
    // The same the other way round.
    assert_eq!(created(&handler, &["-Y", "9"]).await, "16,9 40x6\n");
    assert_eq!(created(&handler, &[]).await, "20,10 40x6\n");
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_refused_opening_leaves_the_cascade_where_it_was() {
    let handler = setup().await;
    assert_eq!(created(&handler, &[]).await, "4,2 40x6\n");
    // Measured: tmux refuses this one with `size or position invalid`, before
    // it places anything, and the memory has not moved when the next float
    // takes its own step.
    let error = refused(&handler, &["-x", "abc"]).await;
    assert!(error.contains("new-pane"), "{error}");
    assert_eq!(created(&handler, &[]).await, "8,4 40x6\n");
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_cascade_belongs_to_the_window_not_to_the_session() {
    let handler = setup().await;
    assert_eq!(created(&handler, &[]).await, "4,2 40x6\n");
    assert_eq!(created(&handler, &[]).await, "8,4 40x6\n");
    queue(&handler, "new-window -d -t fnp")
        .await
        .expect("a second window of the same session");
    // Measured: the other window of the same session starts its own cascade.
    assert_eq!(created(&handler, &["-t", "fnp:1"]).await, "4,2 40x6\n");
    // And the first window keeps its own place in the sequence.
    assert_eq!(created(&handler, &["-t", "fnp:0.0"]).await, "12,6 40x6\n");
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_cascade_starts_again_past_the_window_edge() {
    let handler = setup().await;
    resize(&handler, 20, 8).await;
    // Measured on a 20x8 window, whose default float is 10x2: the four steps
    // that fit, then the two the source still places past the window's own
    // edges (`20,10` then `24,2`), then `4,4`, where both memories have started
    // again — the horizontal one now, the vertical one one step earlier.
    for expected in ["4,2 10x2\n", "8,4 10x2\n", "12,6 10x2\n", "16,8 10x2\n"] {
        assert_eq!(created(&handler, &[]).await, expected);
    }
    // Deliberate divergence, from the placement guard this crate already had:
    // the source never refuses a placement, and puts these two where nothing of
    // the window can show them. RMUX refuses a rectangle whose position has
    // passed an edge, creating nothing — while the memory takes its step all
    // the same, which is what lets the cascade start again instead of proposing
    // the refused position for ever.
    for step in ["20,10", "24,2"] {
        let error = refused(&handler, &[]).await;
        assert!(error.contains("out of bounds"), "{step}: {error}");
    }
    assert_eq!(created(&handler, &[]).await, "4,4 10x2\n");
    assert_eq!(created(&handler, &[]).await, "8,6 10x2\n");
    cleanup(&handler).await;
}

#[tokio::test]
async fn an_explicit_position_out_of_range_is_refused_before_the_cascade_moves() {
    let handler = setup().await;
    // Measured on the pin in a fresh 80x24 window (`fopen1/r2/
    // oracle-r2.json`, cells a01 and a02): `-X 81` and `-Y 25` are refused
    // `size or position too large` with nothing created, and the opening that
    // follows still takes the first step. The source reads the arguments it was
    // given and returns on the first one out of range (`layout.c:1374-1396`)
    // before it walks the memory (1399), so a refused argument costs no step.
    let error = refused_by_the_cli(&handler, &["-X", "81"]).await;
    assert!(error.contains("out of bounds"), "{error}");
    assert_eq!(created(&handler, &[]).await, "4,2 40x6\n");
    // The other axis, once the walk has started: it stays where it was.
    let error = refused_by_the_cli(&handler, &["-Y", "25"]).await;
    assert!(error.contains("out of bounds"), "{error}");
    assert_eq!(created(&handler, &[]).await, "8,4 40x6\n");
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_size_the_window_cannot_hold_is_refused_before_the_cascade_moves() {
    let handler = setup().await;
    // Cells a03 and a04 of the same measurement, through the queue: the
    // admission a sourced file reaches, with the arguments the sizes carry.
    let error = refused(&handler, &["-x", "80"]).await;
    assert!(error.contains("out of bounds"), "{error}");
    assert_eq!(created(&handler, &[]).await, "4,2 40x6\n");
    let error = refused(&handler, &["-y", "24"]).await;
    assert!(error.contains("out of bounds"), "{error}");
    assert_eq!(created(&handler, &[]).await, "8,4 40x6\n");
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_replacement_between_parse_and_commit_never_receives_an_implicit_opening() {
    // The implicit target is resolved while the parse holds the lock, and a
    // queue then waits. Whatever takes that pane's place in between must not
    // receive an opening addressed to its predecessor: the same revalidation an
    // explicit `-t` has always had, for the target nobody wrote.
    for change in ["respawn", "kill", "window"] {
        let handler = setup().await;
        let pid = 842_190;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handler.register_attach(pid, session_name("fnp"), tx).await;
        queue(&handler, "split-window -d -t fnp:0.0")
            .await
            .expect("a neighbour, so killing the current pane leaves the window");
        let pause = install_queue_exact_target_capture_pause(&handler, "new-pane");
        let cloned = handler.clone();
        let pending = tokio::spawn(async move {
            cloned
                .execute_parsed_commands_for_test(
                    pid,
                    CommandParser::new().parse("new-pane -d").unwrap(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), pause.reached.notified())
            .await
            .unwrap();
        match change {
            "respawn" => {
                queue(&handler, "respawn-pane -k -t fnp:0.0").await.unwrap();
            }
            "kill" => {
                queue(&handler, "kill-pane -t fnp:0.0").await.unwrap();
            }
            "window" => {
                queue(&handler, "new-window -dk -t fnp:0").await.unwrap();
            }
            _ => unreachable!(),
        }
        let before = window(&handler).await;
        pause.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(3), pending)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "{change}: {result:?}");
        let after = window(&handler).await;
        assert_eq!(after.panes().len(), before.panes().len(), "{change}");
        assert!(after.floating_stack().is_empty(), "{change}");
        cleanup(&handler).await;
    }
}

#[tokio::test]
async fn two_links_of_one_window_share_its_cascade() {
    let handler = setup().await;
    create_background_identity_session(&handler, session_name("lnk")).await;
    queue(&handler, "link-window -s fnp:0 -t lnk:1")
        .await
        .expect("the same canonical window, reachable under two names");
    // One window, one memory: the second opening walks on from where the first
    // link left it, and the panes are the same panes under both names.
    assert_eq!(created(&handler, &["-t", "fnp:0.0"]).await, "4,2 40x6\n");
    assert_eq!(created(&handler, &["-t", "lnk:1.0"]).await, "8,4 40x6\n");
    let panes = window(&handler).await.pane_count();
    assert_eq!(panes, 3, "both openings landed in the one canonical window");
    // `PartialEq for Window` does not look at the placement memory, so the
    // shared walk is read from where the floats actually went, never from a
    // comparison of two windows.
    assert_eq!(created(&handler, &["-t", "lnk:1.0"]).await, "12,6 40x6\n");
    cleanup(&handler).await;
}
