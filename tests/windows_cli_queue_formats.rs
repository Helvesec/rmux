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
