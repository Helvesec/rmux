//! FNP2 — `new-pane -x N% -y N%`, resolved against the target's window.
//!
//! Every expected size below is a literal measured on the frozen tmux 3.7b
//! (`fnp2/oracle/ORACLE-PERCENT.json`), not a number recomputed
//! from the helper under test. tmux truncates `(D * p) / 100` on integers, so
//! an odd window is where the arithmetic is actually visible: 33% of 79 is 26
//! and 50% of 79 is 39, never 40.
//!
//! One divergence is deliberate and inherited from FNP1: tmux accepts a
//! percentage that rounds to zero and creates a degenerate pane (measured:
//! `-x0%` and `-x1%` give a 0-column pane whose child reports `stty size`
//! `8 0`). RMUX refuses a non-positive rectangle before anything is created.

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

/// Creates a pane and returns what tmux would print for its geometry.
async fn created_geometry(handler: &RequestHandler, target: &str, x: &str, y: &str) -> String {
    let printed = offer(
        handler,
        &[
            "newp",
            "-dP",
            "-t",
            target,
            "-x",
            x,
            "-y",
            y,
            "-X",
            "0",
            "-Y",
            "0",
            "-F",
            "#{pane_floating_flag}|#{pane_width}x#{pane_height}",
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
        } => String::from_utf8(stdout).expect("printed geometry is utf-8"),
        other => panic!("{target} -x{x} -y{y}: {other:?}"),
    }
}

/// The two windows a refusal compares, without what the live shell writes.
///
/// The fixture pane runs a real shell, and that child keeps writing two fields
/// of its own window at any moment: `automatic-rename` names the window after
/// the foreground process (seen on a loaded macOS run: `rmux`, then `bash`
/// once `/bin/sh` has exec'd), and its output stamps the window and pane
/// activity. A refused command produces neither, so the name is blanked on
/// both sides while automatic renaming owns it, and the activity is taken
/// from `before`. Everything else is compared as it is, including whether
/// automatic renaming is still on and an explicit name.
fn without_live_shell_writes(
    before: &rmux_core::Window,
    after: &rmux_core::Window,
) -> (rmux_core::Window, rmux_core::Window) {
    let (mut before, mut after) = (before.clone(), after.clone());
    if before.automatic_rename() && after.automatic_rename() {
        before.set_automatic_name(String::new());
        after.set_automatic_name(String::new());
    }
    for pane in before.panes() {
        let activity = before
            .pane_activity_snapshot(pane.id())
            .expect("a pane of before has its own activity");
        after.apply_pane_activity_snapshot(activity);
    }
    (before, after)
}

/// Lands an automatic rename now, as the live shell may between two snapshots.
pub(super) async fn rename_automatically(handler: &RequestHandler, name: String) {
    let current = window(handler).await;
    assert!(
        current.automatic_rename(),
        "automatic renaming owns the name"
    );
    assert_ne!(current.name(), Some(name.as_str()), "the rename is real");
    handler
        .state
        .lock()
        .await
        .sessions
        .set_automatic_window_name(
            &rmux_proto::WindowTarget::with_window(session_name("fnp"), 0),
            name,
        )
        .expect("the fixture window accepts an automatic name");
}

/// Asserts that a refusal left the fixture window as `before` was, apart from
/// the live shell's own writes. Every refusal on this fixture compares through
/// here, so none of them can go back to equality on the raw Window.
pub(super) async fn assert_refusal_kept_window(
    handler: &RequestHandler,
    before: &rmux_core::Window,
    refused: &str,
) {
    let (before, after) = without_live_shell_writes(before, &window(handler).await);
    assert_eq!(after, before, "{refused} mutated the window");
}

#[tokio::test]
async fn a_percentage_resolves_against_the_window_of_the_resolved_target() {
    let handler = setup().await;
    odd_window(&handler).await;

    // 79 * 33 / 100 = 26 and 25 * 33 / 100 = 8, both truncated.
    assert_eq!(
        created_geometry(&handler, "fnp:0.0", "33%", "33%").await,
        "1|26x8\n"
    );
    // 79 * 50 / 100 = 39, not 40: the odd window is the whole point.
    assert_eq!(
        created_geometry(&handler, "fnp:0.0", "50%", "50%").await,
        "1|39x12\n"
    );
    // 79 * 99 / 100 = 78 and 25 * 99 / 100 = 24, the largest accepted values.
    assert_eq!(
        created_geometry(&handler, "fnp:0.0", "99%", "99%").await,
        "1|78x24\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn one_axis_may_be_cells_while_the_other_is_a_percentage() {
    let handler = setup().await;
    odd_window(&handler).await;
    assert_eq!(
        created_geometry(&handler, "fnp:0.0", "30", "33%").await,
        "1|30x8\n"
    );
    assert_eq!(
        created_geometry(&handler, "fnp:0.0", "33%", "7").await,
        "1|26x7\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_base_is_the_window_and_never_the_target_pane() {
    // The decisive fixture: a target one column wide. A percentage of the pane
    // would round to zero and be refused; a percentage of the window is 40x12.
    let handler = setup().await;
    queue(&handler, "split-window -h -d -l 1 -t fnp:0.0")
        .await
        .expect("split succeeds");
    let before = window(&handler).await;
    assert_eq!(before.pane(1).unwrap().geometry().cols(), 1);
    assert_eq!(before.active_pane_index(), 0, "the target is not active");

    assert_eq!(
        created_geometry(&handler, "fnp:0.1", "50%", "50%").await,
        "1|40x12\n"
    );

    // The same through a command alias, which reaches the same admission.
    queue(
        &handler,
        "set-option -s command-alias[77] 'floatp=new-pane'",
    )
    .await
    .expect("alias set");
    let printed = offer(
        &handler,
        &[
            "floatp", "-d", "-t", "fnp:0.1", "-x50%", "-y50%", "-X0", "-Y0",
        ],
        true,
    )
    .await;
    assert!(
        matches!(
            printed,
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                ..
            }
        ),
        "{printed:?}"
    );
    let aliased = window(&handler).await;
    let created = aliased.pane(3).expect("the alias created a pane");
    assert_eq!(
        (created.geometry().cols(), created.geometry().rows()),
        (40, 12),
        "an alias resolves the percentage the same way"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_percentage_is_resolved_after_the_earlier_commands_of_its_own_queue() {
    // Both commands are admitted one after the other under the same state lock,
    // so the percentage must see the window the resize left behind, not the one
    // the queue started with.
    let handler = setup().await;
    queue(
        &handler,
        "resize-window -t fnp:0 -x 40 -y 12 ; new-pane -d -t fnp:0.0 -x50% -y50% -X0 -Y0",
    )
    .await
    .expect("both commands are admitted");
    let window = window(&handler).await;
    assert_eq!((window.size().cols, window.size().rows), (40, 12));
    let created = window.pane(1).expect("the pane was created");
    assert_eq!(
        (created.geometry().cols(), created.geometry().rows()),
        (20, 6),
        "50% of the resized window, not of the 80x24 it started from"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_refused_percentage_creates_no_pane_no_child_and_no_terminal() {
    let handler = setup().await;
    odd_window(&handler).await;
    // Every case carries valid coordinates, so none of them can stop at the
    // missing-geometry check: each one has to reach the validation it is
    // actually about. Three families, three diagnostics.
    //
    //   * effective zero — 0% and 1% both truncate to zero in a 79-column
    //     window, and the positive rectangle refuses them. tmux accepts both
    //     and creates a degenerate pane; this is the FNP1 divergence kept here.
    //   * whole dimension — 100% reaches D exactly, which the window's own
    //     bound refuses, as tmux's 0..D-1 range does.
    //   * domain and syntax — 101% and the malformed spellings never leave the
    //     percentage parser.
    for (geometry, expected, family) in [
        (
            "-x0% -y33% -X0 -Y0",
            "floating pane dimensions must be positive",
            "zero",
        ),
        (
            "-x1% -y33% -X0 -Y0",
            "floating pane dimensions must be positive",
            "zero",
        ),
        (
            "-x33% -y0% -X0 -Y0",
            "floating pane dimensions must be positive",
            "zero",
        ),
        (
            "-x33% -y1% -X0 -Y0",
            "floating pane dimensions must be positive",
            "zero",
        ),
        (
            "-x100% -y33% -X0 -Y0",
            "floating size or position out of bounds",
            "whole dimension",
        ),
        (
            "-x33% -y100% -X0 -Y0",
            "floating size or position out of bounds",
            "whole dimension",
        ),
        (
            "-x101% -y33% -X0 -Y0",
            "new-pane -x expects a percentage between 0 and 100",
            "domain",
        ),
        (
            "-x33% -y101% -X0 -Y0",
            "new-pane -y expects a percentage between 0 and 100",
            "domain",
        ),
        (
            "-x50%% -y33% -X0 -Y0",
            "new-pane -x expects a percentage",
            "syntax",
        ),
        (
            "-x% -y33% -X0 -Y0",
            "new-pane -x expects a percentage",
            "syntax",
        ),
    ] {
        let before = window(&handler).await;
        // Every case runs through the interleaving that made the full-window
        // comparison flaky: a rename between the two snapshots.
        rename_automatically(&handler, format!("renamed under {geometry}")).await;
        let refused = queue(&handler, &format!("new-pane -d -t fnp:0.0 {geometry}")).await;
        let error = match refused {
            Err(error) => error.to_string(),
            Ok(output) => panic!("{geometry} was accepted: {output:?}"),
        };
        assert!(
            error.contains(expected),
            "{geometry} ({family}) should be refused by {expected:?}, got {error:?}"
        );
        // The refusal must be the one this case is about, not the geometry
        // completeness check standing in for it, and not a target diagnostic.
        assert!(
            !error.contains("requires -X x -Y y"),
            "{geometry} never reached its own validation: {error:?}"
        );
        assert!(
            !error.contains("requires -t pane") && !error.contains("can't find"),
            "{geometry} was refused for its target, not its size: {error:?}"
        );
        assert_refusal_kept_window(&handler, &before, geometry).await;
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

    // The comparison above still sees what it guards: a creation, and an
    // explicit name, each make the two sides differ despite a rename.
    for (accepted, what) in [
        (
            "new-pane -d -t fnp:0.0 -x33% -y33% -X0 -Y0",
            "a created pane",
        ),
        ("rename-window -t fnp:0 explicit", "an explicit name"),
    ] {
        let before = window(&handler).await;
        rename_automatically(&handler, format!("renamed under {accepted}")).await;
        queue(&handler, accepted).await.expect(accepted);
        let (before, after) = without_live_shell_writes(&before, &window(&handler).await);
        assert_ne!(after, before, "{what} must stay visible");
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn positions_keep_their_absolute_spelling_and_resolved_bounds() {
    // FNP3 gave `-X`/`-Y` a percentage spelling; this guards what FNP2 already
    // had. A signed cell coordinate still parses as one, and its bounds still
    // come from the RESOLVED dimension — here a percentage one — so 33% of a
    // 79-column window is 26 cells and -26 is the last legal x.
    let handler = setup().await;
    odd_window(&handler).await;
    for (geometry, valid) in [
        ("-x33% -y33% -X-26 -Y0", true),
        ("-x33% -y33% -X-27 -Y0", false),
        ("-x33% -y33% -X79 -Y0", true),
        ("-x33% -y33% -X80 -Y0", false),
        ("-x33% -y33% -X0 -Y-8", true),
        ("-x33% -y33% -X0 -Y-9", false),
        ("-x33% -y33% -X0 -Y25", true),
        ("-x33% -y33% -X0 -Y26", false),
        ("-x33% -y33% -XNaN -Y0", false),
        ("-x33% -y33% -X0 -Y+3", true),
    ] {
        let before = window(&handler).await;
        rename_automatically(&handler, format!("renamed under {geometry}")).await;
        let result = queue(&handler, &format!("new-pane -d -t fnp:0.0 {geometry}")).await;
        assert_eq!(result.is_ok(), valid, "{geometry}: {result:?}");
        let after = window(&handler).await;
        assert_eq!(
            after.panes().len(),
            before.panes().len() + usize::from(valid),
            "{geometry}"
        );
        if !valid {
            assert_refusal_kept_window(&handler, &before, geometry).await;
        }
    }
    cleanup(&handler).await;
}
