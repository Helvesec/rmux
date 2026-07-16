mod common_cross;

use std::error::Error;

use common_cross::CrossPlatformHarness;

#[test]
fn failed_implicit_web_share_creation_rolls_back_auto_session() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("web-share-auto-session-rollback")?;
    let failed = harness.run([
        "web-share",
        "--ttl",
        "30",
        "--spectator-only",
        "--no-pin",
        "--frontend-url",
        "not-a-url",
    ])?;
    assert!(!failed.status.success(), "invalid frontend URL must fail");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("frontend URL"),
        "failure must occur after the implicit session is allocated: {}",
        String::from_utf8_lossy(&failed.stderr)
    );

    let listed = harness.run(["list-sessions", "-F", "#{session_name}"])?;
    let sessions = String::from_utf8_lossy(&listed.stdout);
    assert!(
        !sessions.lines().any(|name| name.starts_with("web-share-")),
        "failed creation leaked an implicit session: {sessions:?}"
    );
    Ok(())
}
