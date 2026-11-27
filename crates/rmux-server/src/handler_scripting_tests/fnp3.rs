//! FNP3 — `new-pane -X N% -Y N%`, in the canonical window's content frame.
//!
//! A position percentage is `floor(D * p / 100)` where D is the window's width
//! for `-X` and its height for `-Y`, read at the moment the queue has reached.
//! Positions are not dimensions: 0 and D are both legal coordinates here, so
//! the `D - 1` edge that bounds a *size* must not be applied to them, and the
//! float's own size is never subtracted from the frame.
//!
//! The literal expectations come from the arithmetic the FNP2 lot measured on
//! tmux 3.7b for the shared helper — truncation, never rounding — and from the
//! adopted FNP1 `position-percent` observations. Nothing is recomputed here
//! from the code under test.

use super::*;

/// The fixture window is 80x24; this makes it 79x25, odd on both axes.
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

/// Creates a floating pane and returns what tmux would print for it.
async fn created(handler: &RequestHandler, target: &str, geometry: &[&str]) -> String {
    let mut args = vec!["newp", "-dP", "-t", target];
    args.extend_from_slice(geometry);
    args.extend_from_slice(&[
        "-F",
        "#{pane_floating_flag}|#{pane_width}x#{pane_height}@#{pane_left},#{pane_top}",
    ]);
    let printed = offer(handler, &args, true).await;
    match printed {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            exit_status: 0,
            ..
        } => String::from_utf8(stdout).expect("printed geometry is utf-8"),
        other => panic!("{target} {geometry:?}: {other:?}"),
    }
}

#[tokio::test]
async fn a_position_percentage_is_of_the_window_content_frame() {
    let handler = setup().await;
    odd_window(&handler).await;

    // 79 * 50 / 100 = 39 and 25 * 33 / 100 = 8, both truncated.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X50%", "-Y33%"]).await,
        "1|30x8@39,8\n"
    );
    // The same percentage on the other axis: 79 * 33 / 100 = 26,
    // 25 * 50 / 100 = 12.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X33%", "-Y50%"]).await,
        "1|30x8@26,12\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn zero_and_one_hundred_percent_are_coordinates_not_dimensions() {
    let handler = setup().await;
    odd_window(&handler).await;

    // 0% is the origin, and a percentage that truncates to 0 is the same
    // coordinate — unlike a dimension, where zero is refused.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X0%", "-Y1%"]).await,
        "1|30x8@0,0\n"
    );
    // 100% reaches the whole dimension: x = 79 and y = 25 are legal
    // coordinates, so the float lands entirely outside the frame. That is its
    // logical geometry and nothing here pretends it is visible.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X100%", "-Y100%"]).await,
        "1|30x8@79,25\n"
    );
    // The dimension bounds are untouched by all of this: 100% of a *size* is
    // still refused.
    let refused_command = "new-pane -d -t fnp:0.0 -x100% -y8 -X0 -Y0";
    let before = window(&handler).await;
    super::fnp2::rename_automatically(&handler, format!("renamed under {refused_command}")).await;
    let refused = queue(&handler, refused_command).await;
    assert!(refused.is_err(), "{refused:?}");
    super::fnp2::assert_refusal_kept_window(&handler, &before, refused_command).await;
    cleanup(&handler).await;
}

#[tokio::test]
async fn an_axis_may_be_absolute_while_the_other_is_a_percentage() {
    let handler = setup().await;
    odd_window(&handler).await;

    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X-3", "-Y50%"]).await,
        "1|30x8@-3,12\n"
    );
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X50%", "-Y-2"]).await,
        "1|30x8@39,-2\n"
    );
    // A sign is lexical, not a direction: `+50%` is 50% and `-0%` is the
    // origin, not a negative position.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X+50%", "-Y-0%"]).await,
        "1|30x8@39,0\n"
    );
    // Leading zeros are ordinary decimals.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x30", "-y8", "-X050%", "-Y008%"]).await,
        "1|30x8@39,2\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_negative_absolute_offset_is_bounded_by_the_resolved_dimension() {
    // The floor of a negative coordinate is minus the *resolved* width, so a
    // percentage dimension decides it just as a cell count would.
    let handler = setup().await;
    odd_window(&handler).await;

    // 33% of 79 is 26 columns, so -26 is the last legal x and -27 is not.
    assert_eq!(
        created(&handler, "fnp:0.0", &["-x33%", "-y8", "-X-26", "-Y0"]).await,
        "1|26x8@-26,0\n"
    );
    let refused_command = "new-pane -d -t fnp:0.0 -x33% -y8 -X-27 -Y0";
    let before = window(&handler).await;
    super::fnp2::rename_automatically(&handler, format!("renamed under {refused_command}")).await;
    let refused = queue(&handler, refused_command).await;
    assert!(refused.is_err(), "{refused:?}");
    super::fnp2::assert_refusal_kept_window(&handler, &before, refused_command).await;
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_frame_is_the_window_even_for_a_small_or_linked_target() {
    let handler = setup().await;
    odd_window(&handler).await;
    queue(&handler, "split-window -h -d -l 1 -t fnp:0.0")
        .await
        .expect("split succeeds");
    let before = window(&handler).await;
    assert_eq!(before.pane(1).unwrap().geometry().cols(), 1);
    assert_eq!(before.active_pane_index(), 0, "the target is not active");

    // A one-column target: 50% of it would be 0, 50% of its window is 39.
    assert_eq!(
        created(&handler, "fnp:0.1", &["-x30", "-y8", "-X50%", "-Y33%"]).await,
        "1|30x8@39,8\n"
    );
    assert_eq!(
        window(&handler).await.active_pane_index(),
        0,
        "a detached creation does not move the selection"
    );

    // The same window reached through a link resolves the same frame.
    create_background_identity_session(&handler, session_name("linked")).await;
    queue(&handler, "link-window -d -s fnp:0 -t linked:1")
        .await
        .expect("link succeeds");
    assert_eq!(
        created(&handler, "linked:1.0", &["-x30", "-y8", "-X50%", "-Y33%"]).await,
        "1|30x8@39,8\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_position_percentage_is_resolved_after_the_earlier_commands_of_its_queue() {
    let handler = setup().await;
    odd_window(&handler).await;
    queue(
        &handler,
        "resize-window -t fnp:0 -x 83 -y 27 ; new-pane -d -t fnp:0.0 \
         -x30 -y8 -X50% -Y33%",
    )
    .await
    .expect("both commands are admitted");
    let window = window(&handler).await;
    assert_eq!((window.size().cols, window.size().rows), (83, 27));
    let created = window.pane(1).expect("the pane was created");
    // 83 * 50 / 100 = 41 and 27 * 33 / 100 = 8, of the resized window.
    assert_eq!(
        (
            created.geometry().cols(),
            created.geometry().rows(),
            created.geometry().x(),
            created.geometry().y()
        ),
        (30, 8, 41, 8),
        "the percentage saw the window the resize left behind"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_refused_position_creates_no_pane_no_child_and_no_terminal() {
    let handler = setup().await;
    odd_window(&handler).await;
    // Every case carries a complete valid geometry apart from the one value it
    // is about, and a valid target, so each refusal is its own.
    for (geometry, expected, family) in [
        (
            "-x30 -y8 -X101% -Y0",
            "new-pane -X expects a percentage between 0 and 100",
            "domain",
        ),
        (
            "-x30 -y8 -X0 -Y101%",
            "new-pane -Y expects a percentage between 0 and 100",
            "domain",
        ),
        (
            "-x30 -y8 -X-1% -Y0",
            "new-pane: geometry percentage must not be negative",
            "negative percentage",
        ),
        (
            "-x30 -y8 -X0 -Y-50%",
            "new-pane: geometry percentage must not be negative",
            "negative percentage",
        ),
        // Since R2 the lexical shape of a percentage is checked locally and
        // the shared helper owns only its domain, so a malformed percentage
        // names itself the way a malformed absolute already did.
        (
            "-x30 -y8 -X50%% -Y0",
            "new-pane: geometry requires decimal integers",
            "syntax",
        ),
        (
            "-x30 -y8 -X% -Y0",
            "new-pane: geometry requires decimal integers",
            "syntax",
        ),
        (
            "-x30 -y8 -XNaN -Y0",
            "new-pane: geometry requires decimal integers",
            "syntax",
        ),
        (
            "-x30 -y8 -X2147483648 -Y0",
            "new-pane: geometry overflow",
            "overflow",
        ),
    ] {
        let before = window(&handler).await;
        super::fnp2::rename_automatically(&handler, format!("renamed under {geometry}")).await;
        let refused = queue(&handler, &format!("new-pane -d -t fnp:0.0 {geometry}")).await;
        let error = match refused {
            Err(error) => error.to_string(),
            Ok(output) => panic!("{geometry} was accepted: {output:?}"),
        };
        assert!(
            error.contains(expected),
            "{geometry} ({family}) should be refused by {expected:?}, got {error:?}"
        );
        assert!(
            !error.contains("requires -X x -Y y"),
            "{geometry} never reached its own validation: {error:?}"
        );
        assert!(
            !error.contains("requires -t pane") && !error.contains("can't find"),
            "{geometry} was refused for its target: {error:?}"
        );
        super::fnp2::assert_refusal_kept_window(&handler, &before, geometry).await;
        assert!(
            handler
                .state
                .lock()
                .await
                .pane_pid_in_window(&session_name("fnp"), 0, 1)
                .is_err(),
            "{geometry} left a child behind"
        );
        #[cfg(unix)]
        assert!(
            handler
                .state
                .lock()
                .await
                .pane_tty_path_in_window(&session_name("fnp"), 0, 1)
                .is_err(),
            "{geometry} left a terminal behind"
        );
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_percentage_carries_at_most_one_sign() {
    // A sign is lexical, and there is at most one of it. The shared percentage
    // helper parses a `u8`, which accepts a leading `+` of its own, so a value
    // that has already given up one sign must not be allowed to present
    // another: `++50%` and `-+0%` are malformed, not a 50% and an origin.
    let handler = setup().await;
    odd_window(&handler).await;
    for (geometry, family) in [
        ("-x30 -y8 -X++50% -Y0", "two plus signs on X"),
        ("-x30 -y8 -X0 -Y++50%", "two plus signs on Y"),
        ("-x30 -y8 -X-+0% -Y0", "minus then plus on X"),
        ("-x30 -y8 -X0 -Y-+0%", "minus then plus on Y"),
        ("-x30 -y8 -X+-0% -Y0", "plus then minus on X"),
        ("-x30 -y8 -X0 -Y--50%", "two minus signs on Y"),
    ] {
        let before = window(&handler).await;
        super::fnp2::rename_automatically(&handler, format!("renamed under {geometry}")).await;
        let refused = queue(&handler, &format!("new-pane -d -t fnp:0.0 {geometry}")).await;
        let error = match refused {
            Err(error) => error.to_string(),
            Ok(output) => panic!("{geometry} ({family}) was accepted: {output:?}"),
        };
        assert!(
            error.contains("new-pane: geometry requires decimal integers"),
            "{geometry} ({family}) should name its malformed value, got {error:?}"
        );
        assert!(
            !error.contains("requires -X x -Y y"),
            "{geometry} never reached its own validation: {error:?}"
        );
        assert!(
            !error.contains("requires -t pane") && !error.contains("can't find"),
            "{geometry} was refused for its target: {error:?}"
        );
        super::fnp2::assert_refusal_kept_window(&handler, &before, geometry).await;
        assert!(
            handler
                .state
                .lock()
                .await
                .pane_pid_in_window(&session_name("fnp"), 0, 1)
                .is_err(),
            "{geometry} left a child behind"
        );
        #[cfg(unix)]
        assert!(
            handler
                .state
                .lock()
                .await
                .pane_tty_path_in_window(&session_name("fnp"), 0, 1)
                .is_err(),
            "{geometry} left a terminal behind"
        );
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn one_sign_and_leading_zeros_stay_legal() {
    // The neighbours of the malformed spellings above, which must keep working:
    // one sign, any number of leading zeros, and the signed zero that is the
    // origin rather than a negative position.
    let handler = setup().await;
    odd_window(&handler).await;
    for (geometry, expected) in [
        (["-x30", "-y8", "-X+50%", "-Y0%"], "1|30x8@39,0\n"),
        (["-x30", "-y8", "-X-0%", "-Y33%"], "1|30x8@0,8\n"),
        (["-x30", "-y8", "-X-000%", "-Y0"], "1|30x8@0,0\n"),
        (["-x30", "-y8", "-X0050%", "-Y008%"], "1|30x8@39,2\n"),
        (["-x30", "-y8", "-X+0%", "-Y+100%"], "1|30x8@0,25\n"),
    ] {
        assert_eq!(
            created(&handler, "fnp:0.0", &geometry).await,
            expected,
            "{geometry:?}"
        );
    }
    cleanup(&handler).await;
}
