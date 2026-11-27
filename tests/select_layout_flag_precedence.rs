#![cfg(unix)]

mod common;

use std::error::Error;

use common::{assert_success, stderr, stdout, CliHarness};

fn pane_geometry(harness: &CliHarness) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&[
        "list-panes",
        "-t",
        "alpha:0",
        "-F",
        "#{pane_index}:#{pane_left},#{pane_top},#{pane_width}x#{pane_height}",
    ])?;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(stderr(&output).is_empty(), "{output:?}");
    Ok(stdout(&output))
}

#[test]
fn cli_select_layout_en_uses_tmux_priority_and_preserves_queue_semantics(
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("select-layout-en-priority")?;
    let _daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-x",
        "80",
        "-y",
        "24",
        "sleep 60",
    ])?);
    assert_success(&harness.run(&["split-window", "-h", "-d", "-t", "alpha:0.0", "sleep 60"])?);
    assert_success(&harness.run(&["select-layout", "-t", "alpha:0", "even-horizontal"])?);

    assert_success(&harness.run(&[
        "select-layout",
        "-En",
        "-t",
        "alpha:0",
        ";",
        "set-buffer",
        "-b",
        "select-layout-cli-first",
        "ok",
    ])?);
    let first_geometry = pane_geometry(&harness)?;
    assert!(
        first_geometry.lines().all(|line| line.contains(",80x")),
        "tmux 3.7b oracle cell c195 requires a full-width vertical layout: {first_geometry:?}"
    );
    assert!(
        first_geometry.starts_with("0:0,0,") && first_geometry.contains("\n1:0,"),
        "the first -En application must choose the next, top-to-bottom layout: {first_geometry:?}"
    );
    let first_canary = harness.run(&["show-buffer", "-b", "select-layout-cli-first"])?;
    assert_eq!(first_canary.status.code(), Some(0), "{first_canary:?}");
    assert_eq!(stdout(&first_canary), "ok");

    assert_success(&harness.run(&[
        "selectl",
        "-nE",
        "-t",
        "alpha:0",
        ";",
        "set-buffer",
        "-b",
        "select-layout-cli-repeat",
        "ok",
    ])?);
    let repeated_geometry = pane_geometry(&harness)?;
    assert_eq!(
        repeated_geometry, "0:0,0,80x22\n1:0,23,80x1\n",
        "tmux 3.7b oracle cell d89e must govern the repeated cluster"
    );
    let repeated_canary = harness.run(&["show-buffer", "-b", "select-layout-cli-repeat"])?;
    assert_eq!(
        repeated_canary.status.code(),
        Some(0),
        "{repeated_canary:?}"
    );
    assert_eq!(stdout(&repeated_canary), "ok");

    let rejected = harness.run(&[
        "select-layout",
        "-Enx",
        "-t",
        "alpha:0",
        ";",
        "set-buffer",
        "-b",
        "select-layout-cli-rejected",
        "must-not-run",
    ])?;
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(
        stderr(&rejected).contains("command select-layout: unknown flag -x"),
        "unexpected clustered unknown-option error: {rejected:?}"
    );
    assert_eq!(
        pane_geometry(&harness)?,
        repeated_geometry,
        "an invalid cluster must not mutate the layout"
    );
    let rejected_canary = harness.run(&["show-buffer", "-b", "select-layout-cli-rejected"])?;
    assert_eq!(
        rejected_canary.status.code(),
        Some(1),
        "a command after a rejected cluster must not execute: {rejected_canary:?}"
    );

    Ok(())
}
