//! FNP4 — `new-pane`/`newp` with `-x`, `-y` or both omitted.
//!
//! Every expected size below is a literal that was measured on the pinned
//! tmux 3.7b, never recomputed from the helper under test:
//!
//! * the 80x24 window giving a 40x6 float is the adopted FNP1 receipt
//!   (`runs/fnp2/adopted/native-01`, `commands.json[163]` /
//!   `observations.json[15]`, label `default-geometry`, float `%29`, PID 81903,
//!   child PTY 40x6). That cell omitted the positions too; only its size is
//!   adopted here, because `-X`/`-Y` stay required.
//! * the odd, mixed and zero cells are this lot's own targeted oracle
//!   (`fnp4/oracle`), on the same frozen binary that produced the
//!   receipt above: 79x25 gives 39x6, `-x 30` gives 30x6, `-y 8` gives 39x8,
//!   `-y 33%` gives 39x8, and an 80x24 window with explicit `-X 5 -Y 3` still
//!   gives 40x6 at (5,3).
//!
//! tmux divides on integers and replaces each axis only if its own flag was
//! given (`layout.c:1371`), so the two axes are independent and 79/2 is 39.
//!
//! One divergence is deliberate and inherited from FNP1 and FNP2. Measured on
//! the same binary: an 80x3 window gives a 40x**0** float and a 1x24 window a
//! **0**x6 float, both created with exit 0. RMUX refuses a non-positive
//! rectangle before anything is created, and this lot keeps that.

use super::*;

/// Makes the 80x24 fixture odd on both axes.
async fn odd_window(handler: &RequestHandler) {
    queue(handler, "resize-window -t fnp:0 -x 79 -y 25")
        .await
        .expect("the detached window resizes");
    let window = window(handler).await;
    assert_eq!(
        (window.size().cols, window.size().rows),
        (79, 25),
        "the fixture must really be odd on both axes"
    );
}

/// Creates a pane with the given geometry arguments and returns what tmux
/// would print for it.
async fn created(handler: &RequestHandler, target: &str, geometry: &[&str]) -> String {
    let mut args = vec!["newp", "-dP", "-t", target];
    args.extend_from_slice(geometry);
    args.extend_from_slice(&[
        "-F",
        "#{pane_floating_flag}|#{pane_width}x#{pane_height}|#{pane_left},#{pane_top}",
    ]);
    match offer(handler, &args, true).await {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            exit_status: 0,
            ..
        } => String::from_utf8(stdout).expect("printed geometry is utf-8"),
        other => panic!("{target} {geometry:?}: {other:?}"),
    }
}

/// Attempts a creation that must be refused, and returns the message.
///
/// The refusal is read through the queue the neighbouring FNP suites use, so
/// the message is the admission's own and the pane count is checked on the
/// same window the command addressed.
async fn refused(handler: &RequestHandler, target: &str, geometry: &[&str]) -> String {
    let before = window(handler).await;
    let command = format!("new-pane -d -t {target} {}", geometry.join(" "));
    let error = match queue(handler, &command).await {
        Err(error) => error.to_string(),
        Ok(output) => panic!("{command} was accepted: {output:?}"),
    };
    let after = window(handler).await;
    assert_eq!(
        after.pane_count(),
        before.pane_count(),
        "{geometry:?} created a pane before it was refused: {error}"
    );
    for old in before.panes() {
        assert_eq!(after.pane(old.index()).unwrap().id(), old.id());
        assert_eq!(after.pane(old.index()).unwrap().geometry(), old.geometry());
    }
    error.to_string()
}

#[tokio::test]
async fn omitting_both_dimensions_takes_half_the_window_and_a_quarter_of_it() {
    let handler = setup().await;
    // The adopted receipt's cell: an 80x24 window gives 40x6. The positions are
    // explicit here because FNP4 does not touch them.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-X", "0", "-Y", "0"]).await,
        "1|40x6|0,0\n"
    );
    // Measured cell g: the same size with the float placed elsewhere.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-X", "5", "-Y", "3"]).await,
        "1|40x6|5,3\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn each_axis_may_be_omitted_on_its_own() {
    let handler = setup().await;
    odd_window(&handler).await;
    // Measured cell a: both omitted on an odd window, 79/2 and 25/4 truncated.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-X", "0", "-Y", "0"]).await,
        "1|39x6|0,0\n"
    );
    // Measured cell b: an explicit width keeps the defaulted height.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x", "30", "-X", "0", "-Y", "0"]).await,
        "1|30x6|0,0\n"
    );
    // Measured cell c: an explicit height keeps the defaulted width.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-y", "8", "-X", "0", "-Y", "0"]).await,
        "1|39x8|0,0\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_defaulted_axis_mixes_with_an_explicit_percentage() {
    let handler = setup().await;
    odd_window(&handler).await;
    // Measured cell d: the width defaults while the height is 33% of 25.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-y", "33%", "-X", "0", "-Y", "0"]).await,
        "1|39x8|0,0\n"
    );
    // The mirror, from the same arithmetic: 33% of 79 is 26, and the height
    // defaults to 25/4.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x", "33%", "-X", "0", "-Y", "0"]).await,
        "1|26x6|0,0\n"
    );
    // A percentage position is still a position: the defaults do not touch it.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-X", "50%", "-Y", "0"]).await,
        "1|39x6|39,0\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_defaults_come_from_the_window_and_never_from_the_target_pane() {
    let handler = setup().await;
    queue(&handler, "split-window -h -d -l 1 -t fnp:0.0")
        .await
        .expect("split succeeds");
    let before = window(&handler).await;
    assert_eq!(before.pane(1).unwrap().geometry().cols(), 1);
    assert_eq!(before.active_pane_index(), 0, "the target is not active");

    // Half of the one-column target would be zero and be refused; half of its
    // 80-column window is 40.
    assert_eq!(
        created(&handler, "fnp:0.1", &["-X", "0", "-Y", "0"]).await,
        "1|40x6|0,0\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_resize_earlier_in_the_same_queue_is_what_the_defaults_read() {
    let handler = setup().await;
    // One parsed invocation: the resize and the creation are admitted by the
    // same queue, so the defaults must read the size the queue has reached.
    queue(
        &handler,
        "resize-window -t fnp:0 -x 100 -y 40 ; newp -dP -t fnp:0.0 -X 0 -Y 0 \
         -F '#{pane_width}x#{pane_height}'",
    )
    .await
    .expect("the queued resize and creation succeed");
    let window = window(&handler).await;
    assert_eq!((window.size().cols, window.size().rows), (100, 40));
    let created = window.pane(1).expect("the float exists").geometry();
    assert_eq!(
        (created.cols(), created.rows()),
        (50, 10),
        "the defaults read the resized window, not the one the queue started with"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_positions_are_no_longer_required() {
    let handler = setup().await;
    // FOPEN1 replaced the refusal this test used to pin: an omitted axis is
    // filled by the window's own cascade. The rule is the measured one - each
    // axis walks alone, four columns and two rows at a time, starting at 4 and
    // 2 (`fopen1/native/oracle-fopen1.json`, cells c01 to c06) -
    // and these four forms are that rule applied in this fixture.
    for (geometry, expected) in [
        (vec!["-X", "0"], "1|40x6|0,2\n"),
        (vec!["-Y", "0"], "1|40x6|4,0\n"),
        (vec!["-x", "10", "-y", "4"], "1|10x4|8,4\n"),
        (vec![], "1|40x6|12,6\n"),
    ] {
        assert_eq!(created(&handler, "fnp:0.0", &geometry).await, expected);
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_default_that_truncates_to_zero_is_refused_before_anything_is_created() {
    let handler = setup().await;
    // Measured on the pinned binary: an 80x3 window gives tmux a 40x0 float,
    // created with exit 0. RMUX refuses it, as FNP1 and FNP2 chose, and the
    // refusal happens before any pane, PTY or process exists.
    queue(&handler, "resize-window -t fnp:0 -x 80 -y 3")
        .await
        .expect("the detached window resizes");
    let before = window(&handler).await;
    assert_eq!((before.size().cols, before.size().rows), (80, 3));

    let error = refused(&handler, "fnp:0.0", &["-X", "0", "-Y", "0"]).await;
    assert!(
        error.contains("floating pane dimensions must be positive"),
        "the zero height was refused by something else: {error}"
    );

    // The same on the other axis: a 1-column window halves to zero. Measured
    // on the pinned binary as a 0x6 float, also created with exit 0.
    queue(&handler, "resize-window -t fnp:0 -x 1 -y 24")
        .await
        .expect("the detached window resizes");
    let error = refused(&handler, "fnp:0.0", &["-X", "0", "-Y", "0"]).await;
    assert!(
        error.contains("floating pane dimensions must be positive"),
        "the zero width was refused by something else: {error}"
    );

    // An explicit dimension on the healthy axis does not rescue the other.
    let error = refused(&handler, "fnp:0.0", &["-y", "6", "-X", "0", "-Y", "0"]).await;
    assert!(
        error.contains("floating pane dimensions must be positive"),
        "the zero width was refused by something else: {error}"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_long_spelling_and_the_alias_reach_the_same_admission() {
    let handler = setup().await;
    let printed = offer(
        &handler,
        &[
            "new-pane",
            "-dP",
            "-t",
            "fnp:0.0",
            "-X",
            "0",
            "-Y",
            "0",
            "-F",
            "#{pane_width}x#{pane_height}",
        ],
        true,
    )
    .await;
    match printed {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            exit_status: 0,
            ..
        } => assert_eq!(String::from_utf8(stdout).unwrap(), "40x6\n"),
        other => panic!("new-pane with defaults: {other:?}"),
    }
    // The alias, through the parsed queue rather than the runtime request.
    queue(&handler, "newp -d -t fnp:0.0 -X 1 -Y 1")
        .await
        .expect("the alias accepts the omitted dimensions");
    let window = window(&handler).await;
    let created = window.pane(2).expect("the second float exists").geometry();
    assert_eq!((created.cols(), created.rows()), (40, 6));
    cleanup(&handler).await;
}
