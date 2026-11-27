#![cfg(unix)]

mod common;

use common::{assert_success, stderr, stdout, CliHarness};
use std::error::Error;

#[test]
fn quiet_only_if_unset_cli_preserves_value_and_after_hook() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("quiet-only-unset")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "quiet"])?);
    assert_success(&harness.run(&["set-option", "-g", "base-index", "5"])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "after-set-option",
        "set-option -g @after-quiet yes",
    ])?);
    assert_success(&harness.run(&[
        "set-hook",
        "-g",
        "command-error",
        "set-option -g @quiet-error yes",
    ])?);

    let output = harness.run(&["set-option", "-g", "-o", "-q", "base-index", "7"])?;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    let value = harness.run(&["show-options", "-gv", "base-index"])?;
    assert_eq!(value.status.code(), Some(0));
    assert!(stderr(&value).is_empty());
    assert_eq!(stdout(&value), "5\n");
    let hook = harness.run(&["show-options", "-gv", "@after-quiet"])?;
    assert_eq!(hook.status.code(), Some(0));
    assert!(stderr(&hook).is_empty());
    assert_eq!(stdout(&hook), "yes\n");
    let error_hook = harness.run(&["show-options", "-gqv", "@quiet-error"])?;
    assert_success(&error_hook);
    assert!(stdout(&error_hook).is_empty());
    Ok(())
}

#[test]
fn quiet_only_if_unset_window_cli_preserves_value_and_errors() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("quiet-window-unset")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "quiet"])?);
    assert_success(&harness.run(&["set-window-option", "-g", "pane-base-index", "5"])?);
    let output = harness.run(&["set-window-option", "-g", "-oq", "pane-base-index", "7"])?;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
    let value = harness.run(&["show-options", "-gwv", "pane-base-index"])?;
    assert_eq!(value.status.code(), Some(0));
    assert!(stderr(&value).is_empty());
    assert_eq!(stdout(&value), "5\n");

    let error = harness.run(&[
        "set-window-option",
        "-t",
        "quiet:0",
        "-oq",
        "window-size",
        "invalid-size",
    ])?;
    assert_eq!(error.status.code(), Some(1));
    assert!(stdout(&error).is_empty());
    assert_eq!(stderr(&error), "unknown value: invalid-size\n");
    Ok(())
}

#[test]
fn quiet_only_if_unset_cli_preserves_decoded_scalar_arguments() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("quiet-literal-unset")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "quiet"])?);
    for (command, show_flags) in [("set-option", "-gv"), ("set-window-option", "-gwv")] {
        for (index, (argument, expected)) in [
            (r"tail\;", "tail;"),
            (r"\;", ";"),
            (r"tail\\;", r"tail\;"),
            (r#"C:\tools\new\$HOME'"#, r#"C:\tools\new\$HOME'"#),
        ]
        .into_iter()
        .enumerate()
        {
            let name = format!("@quiet-literal-{index}");
            let output = harness.run(&[command, "-g", "-oq", &name, argument])?;
            assert_success(&output);
            let value = harness.run(&["show-options", show_flags, &name])?;
            assert_eq!(value.status.code(), Some(0));
            assert!(stderr(&value).is_empty());
            assert_eq!(
                stdout(&value),
                format!("{expected}\n"),
                "{command}: {argument:?}"
            );
        }
    }
    Ok(())
}
