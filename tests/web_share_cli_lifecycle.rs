mod common_cross;

use std::error::Error;
use std::net::TcpListener;

use common_cross::{assert_success, CrossPlatformHarness};

#[test]
fn queued_start_server_applies_its_web_options() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("queued-start-server-web-options")?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let port_text = port.to_string();

    let started = harness.run([
        "list-commands",
        ";",
        "start-server",
        "--web-port",
        &port_text,
        "--frontend-url",
        "https://queued.example.invalid/share",
    ])?;
    assert_success(&started)?;

    let config = harness.stdout(["web-share", "--config"])?;
    assert_eq!(
        config,
        format!("127.0.0.1:{port} https://queued.example.invalid\n")
    );
    Ok(())
}

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
