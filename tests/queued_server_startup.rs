mod common_cross;

use std::error::Error;

use common_cross::{assert_success, CrossPlatformHarness};

#[test]
fn cold_queue_applies_options_before_creating_its_session() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("cold-queue-options")?;

    let started = harness.run([
        "set-option",
        "-g",
        "status",
        "off",
        ";",
        "new-session",
        "-d",
        "-s",
        "main",
    ])?;
    assert_success(&started)?;
    assert_eq!(harness.stdout(["show-options", "-gv", "status"])?, "off\n");
    harness.success(["has-session", "-t", "main"])?;
    Ok(())
}
