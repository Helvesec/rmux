use super::characterization::attach;
use super::sdk::{runtime, sdk, SdkBinary};
use super::*;
use std::path::Path;

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}
fn wait_bytes(path: &Path, expected: &[u8]) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    while bytes(path) != expected {
        assert!(
            Instant::now() < deadline,
            "{}: {:?}, expected {expected:?}",
            path.display(),
            bytes(path)
        );
        thread::sleep(Duration::from_millis(10));
    }
}
fn workload(path: &Path) -> String {
    format!(
        "stty raw -echo; printf '\\033[?1004hFOCUS-READY'; cat > '{}'",
        path.display()
    )
}
fn ready(harness: &CliHarness, target: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        let output = harness.run(&["capture-pane", "-p", "-t", target])?;
        if common::stdout(&output).contains("FOCUS-READY") {
            return Ok(());
        }
        assert!(
            Instant::now() < deadline,
            "pane {target} did not enable focus reports"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
fn select(
    harness: &CliHarness,
    through_sdk: bool,
    command: &str,
    target: &str,
) -> Result<bool, Box<dyn Error>> {
    if !through_sdk {
        return Ok(harness.run(&[command, "-t", target])?.status.success());
    }
    runtime().block_on(async {
        let rmux = sdk(harness).await?;
        if command == "select-window" {
            let index = target.strip_prefix("alpha:").unwrap().parse()?;
            let session = rmux.session(rmux_sdk::SessionName::new("alpha")?).await?;
            Ok(session.window(index).select().await.is_ok())
        } else {
            Ok(rmux.cmd([command, "-t", target]).await?.exit == Some(0))
        }
    })
}
fn characterize(through_sdk: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new(if through_sdk {
        "sdk-focus"
    } else {
        "cli-focus"
    })?;
    let _daemon = harness.start_hidden_daemon()?;
    let _binary = SdkBinary::pin();
    let paths = ["first", "second", "window"].map(|name| harness.tmpdir().join(name));
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", &workload(&paths[0])])?);
    assert_success(&harness.run(&["set-option", "-g", "focus-events", "on"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0", &workload(&paths[1])])?);
    assert_success(&harness.run(&["new-window", "-d", "-t", "alpha:1", &workload(&paths[2])])?);
    for target in ["alpha:0.0", "alpha:0.1", "alpha:1.0"] {
        ready(&harness, target)?;
    }
    let mut attached = attach(&harness, "alpha")?;
    wait_bytes(&paths[0], b"\x1b[I");
    assert!(select(&harness, through_sdk, "select-pane", "alpha:0.1")?);
    wait_bytes(&paths[0], b"\x1b[I\x1b[O");
    wait_bytes(&paths[1], b"\x1b[I");
    let before = paths.each_ref().map(|path| bytes(path));
    assert!(select(&harness, through_sdk, "select-pane", "alpha:0.1")?);
    assert!(!select(&harness, through_sdk, "select-pane", "%999999")?);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        paths.each_ref().map(|path| bytes(path)),
        before,
        "reselect/reject has no focus delta"
    );
    assert!(select(&harness, through_sdk, "select-window", "alpha:1")?);
    wait_bytes(&paths[1], b"\x1b[I\x1b[O");
    wait_bytes(&paths[2], b"\x1b[I");
    let before = paths.each_ref().map(|path| bytes(path));
    assert!(select(&harness, through_sdk, "select-window", "alpha:1")?);
    assert!(!select(&harness, through_sdk, "select-window", "alpha:99")?);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        paths.each_ref().map(|path| bytes(path)),
        before,
        "window reselect/reject has no focus delta"
    );
    attached.send_bytes(b"\x02d")?;
    assert!(attached.wait_for_exit(PROCESS_TIMEOUT)?.success());
    Ok(())
}
#[test]
fn l05_1_cli_focus_delta_on_reject_and_reselect() -> Result<(), Box<dyn Error>> {
    characterize(false)
}
#[test]
fn l05_1_sdk_window_select_and_cmd_pane_focus() -> Result<(), Box<dyn Error>> {
    characterize(true)
}
