#![cfg(windows)]

mod common_cross;

use std::error::Error;

use common_cross::CrossPlatformHarness;

#[test]
fn display_message_queued_path_preserves_dollar_regex_anchor() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-queued-dollar-anchor")?;

    harness.success(["new-session", "-d", "-s", "alpha"])?;

    let format = "#{s/$/Z/:session_name}";
    let direct = harness.stdout(["display-message", "-p", format])?;
    let queued = harness.stdout(["display-message", "-p", "-t", "alpha:0.0", format])?;

    assert_eq!(direct.trim(), "alphaZ");
    assert_eq!(queued, direct);
    Ok(())
}

#[test]
fn display_message_queued_path_preserves_windows_quoting_and_dollar_anchor(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("windows-queued-path-quoting")?;

    harness.success(["new-session", "-d", "-s", "alpha"])?;

    let format = r##"C:\Users\RMUX User\quoted "dir"\#{s/$/Z/:session_name}"##;
    let direct = harness.stdout(["display-message", "-p", format])?;
    let queued = harness.stdout(["display-message", "-p", "-t", "alpha:0.0", format])?;

    assert_eq!(direct, "C:\\Users\\RMUX User\\quoted \"dir\"\\alphaZ\n");
    assert_eq!(queued, direct);
    Ok(())
}

#[test]
fn set_option_unset_scope_matrix_matches_tmux() -> Result<(), Box<dyn Error>> {
    // Windows twin of cli_surface::set_option_unset_scope_matrix_matches_tmux.
    // Oracle probe 2026-07-09 (pinned tmux 3.7b): with @agent.state set at
    // session, window, and pane scopes, plain `set -U` unsets the session
    // copy only; `set -pU` unsets the pane copy only; `set -wU` unsets the
    // window copy and clears the window's pane overrides.
    let harness = CrossPlatformHarness::new("windows-set-option-unset-scope-matrix")?;

    harness.success(["new-session", "-d", "-s", "alpha"])?;
    let set_all = |harness: &CrossPlatformHarness| -> Result<(), Box<dyn Error>> {
        harness.success(["set-option", "-t", "alpha", "@agent.state", "session"])?;
        harness.success([
            "set-option",
            "-w",
            "-t",
            "alpha:0",
            "@agent.state",
            "window",
        ])?;
        harness.success([
            "set-option",
            "-p",
            "-t",
            "alpha:0.0",
            "@agent.state",
            "pane",
        ])?;
        Ok(())
    };
    let values =
        |harness: &CrossPlatformHarness| -> Result<(String, String, String), Box<dyn Error>> {
            Ok((
                harness
                    .stdout(["show-options", "-qv", "-t", "alpha", "@agent.state"])?
                    .trim_end()
                    .to_owned(),
                harness
                    .stdout(["show-options", "-wqv", "-t", "alpha:0", "@agent.state"])?
                    .trim_end()
                    .to_owned(),
                harness
                    .stdout(["show-options", "-pqv", "-t", "alpha:0.0", "@agent.state"])?
                    .trim_end()
                    .to_owned(),
            ))
        };

    set_all(&harness)?;
    harness.success(["set-option", "-U", "-t", "alpha:0.0", "@agent.state"])?;
    assert_eq!(
        values(&harness)?,
        (String::new(), "window".to_owned(), "pane".to_owned()),
        "plain -U unsets the session copy only"
    );

    set_all(&harness)?;
    harness.success(["set-option", "-pU", "-t", "alpha:0.0", "@agent.state"])?;
    assert_eq!(
        values(&harness)?,
        ("session".to_owned(), "window".to_owned(), String::new()),
        "-pU unsets the pane copy only"
    );

    set_all(&harness)?;
    harness.success(["set-option", "-wU", "-t", "alpha:0", "@agent.state"])?;
    assert_eq!(
        values(&harness)?,
        ("session".to_owned(), String::new(), String::new()),
        "-wU unsets the window copy and clears pane overrides"
    );

    Ok(())
}
