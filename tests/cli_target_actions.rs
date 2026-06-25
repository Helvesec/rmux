#![cfg(unix)]

use std::error::Error;

mod common;

use common::{assert_success, stderr, stdout, CliHarness};

#[test]
fn cli_target_actions_run_split_resize_and_capture() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("cli-target-actions")?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"])?);
    assert_success(&harness.run(&["split-window", "-h", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["resize-pane", "-t", "alpha:0.1", "-x", "30"])?);

    let capture = harness.run(&["capture-pane", "-p", "-t", "alpha:0.1"])?;
    assert_eq!(capture.status.code(), Some(0), "capture-pane failed");
    assert!(stderr(&capture).is_empty(), "stderr: {}", stderr(&capture));

    Ok(())
}

#[test]
fn cli_target_action_errors_keep_tmux_target_shape() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("cli-target-action-errors")?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    for argv in [
        &["split-window", "-t", "alpha:0.99"][..],
        &["resize-pane", "-t", "alpha:0.99", "-x", "10"],
        &["capture-pane", "-p", "-t", "alpha:0.99"],
    ] {
        let output = harness.run(argv)?;
        assert_eq!(
            output.status.code(),
            Some(1),
            "{argv:?} unexpectedly succeeded with stdout {:?}",
            stdout(&output)
        );
        assert!(
            stdout(&output).is_empty(),
            "{argv:?} produced stdout {:?}",
            stdout(&output)
        );
        assert_eq!(stderr(&output), "can't find pane: 99\n", "{argv:?}");
    }

    Ok(())
}
