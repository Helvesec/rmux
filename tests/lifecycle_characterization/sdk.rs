use super::characterization::{attach, client_name, transcript};
use super::*;
use rmux_sdk::{PaneRef, Rmux, SessionName};
use std::ffi::OsString;

pub(super) struct SdkBinary(Option<OsString>);
impl SdkBinary {
    pub(super) fn pin() -> Self {
        let old = std::env::var_os("RMUX_SDK_DAEMON_BINARY");
        std::env::set_var("RMUX_SDK_DAEMON_BINARY", env!("CARGO_BIN_EXE_rmux"));
        Self(old)
    }
}
impl Drop for SdkBinary {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("RMUX_SDK_DAEMON_BINARY", value),
            None => std::env::remove_var("RMUX_SDK_DAEMON_BINARY"),
        }
    }
}
pub(super) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("SDK runtime")
}
pub(super) async fn sdk(harness: &CliHarness) -> Result<Rmux, Box<dyn Error>> {
    Ok(Rmux::builder()
        .unix_socket(harness.socket_path())
        .default_timeout(PROCESS_TIMEOUT)
        .connect()
        .await?)
}
async fn cmd(rmux: &Rmux, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let result = rmux.cmd(args).await?;
    assert_eq!(result.exit, Some(0), "{args:?}: {:?}", result.stderr);
    Ok(String::from_utf8(result.stdout)?)
}
async fn wait_text(pane: &rmux_sdk::Pane, expected: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        let text = pane.snapshot().await?.visible_text();
        if text.contains(expected) {
            return Ok(());
        }
        assert!(Instant::now() < deadline, "missing {expected:?}: {text:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn l05_1_sdk_detach_escape_hatch() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sdk-detach-characterization")?;
    let _daemon = harness.start_hidden_daemon()?;
    let _binary = SdkBinary::pin();
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 120"])?);
    let mut attached = attach(&harness, "alpha")?;
    let name = client_name(&harness, attached.child_mut().id())?;
    let mut observer = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
    observer.wait_for_output("%session-changed ")?;
    runtime().block_on(async {
        cmd(&sdk(&harness).await?, &["detach-client", "-t", &name]).await?;
        Ok::<_, Box<dyn Error>>(())
    })?;
    assert!(attached.wait_for_exit(PROCESS_TIMEOUT)?.success());
    observer.wait_for_output(&format!("%client-detached {name}\n"))?;
    observer.write_input("display-message -p DETACH-DONE\n")?;
    observer.wait_for_output("DETACH-DONE\n")?;
    assert_eq!(
        transcript(&observer)
            .lines()
            .filter(|line| *line == format!("%client-detached {name}"))
            .count(),
        1
    );
    assert_success(&harness.run(&["has-session", "-t", "alpha"])?);
    Ok(())
}

#[test]
fn l05_1_sdk_cmd_rename_named_vs_stable_handle() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sdk-rekey-characterization")?;
    let _daemon = harness.start_hidden_daemon()?;
    let _binary = SdkBinary::pin();
    runtime().block_on(async {
        let rmux = sdk(&harness).await?;
        cmd(
            &rmux,
            &["new-session", "-d", "-s", "alpha", "stty -echo; cat"],
        )
        .await?;
        let alpha = SessionName::new("alpha")?;
        let named = rmux.pane(PaneRef::new(alpha.clone(), 0, 0)).await?;
        let id = named.id().await?.expect("original pane");
        let stable = rmux.pane_by_id(alpha.clone(), id).await?;
        cmd(&rmux, &["rename-session", "-t", "alpha", "renamed"]).await?;
        cmd(
            &rmux,
            &["new-session", "-d", "-s", "alpha", "stty -echo; cat"],
        )
        .await?;
        let replacement = named.id().await?.expect("named slot follows replacement");
        assert_ne!(replacement, id);
        stable.send_text("STABLE-ORIGINAL\n").await?;
        wait_text(&stable, "STABLE-ORIGINAL").await?;
        assert_eq!(stable.id().await?, Some(id));
        assert!(!named
            .snapshot()
            .await?
            .visible_text()
            .contains("STABLE-ORIGINAL"));
        assert_eq!(named.id().await?, Some(replacement));
        Ok(())
    })
}

#[test]
fn l05_1_sdk_ordinary_pane_drop_is_inert() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sdk-drop-characterization")?;
    let _daemon = harness.start_hidden_daemon()?;
    let _binary = SdkBinary::pin();
    runtime().block_on(async {
        let rmux = sdk(&harness).await?;
        cmd(&rmux, &["new-session", "-d", "-s", "alpha", "sleep 120"]).await?;
        let session = rmux.session(SessionName::new("alpha")?).await?;
        let pane = session.pane(0, 0);
        let id = pane.id().await?;
        pane.clone().detach();
        drop(pane);
        assert_eq!(session.pane(0, 0).id().await?, id);
        assert!(session.kill().await?);
        assert!(!session.pane(0, 0).exists().await?);
        Ok(())
    })
}

#[test]
fn l05_1_sdk_awaited_per_pane_order_and_cmd_queue() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("sdk-fifo-characterization")?;
    let _daemon = harness.start_hidden_daemon()?;
    let _binary = SdkBinary::pin();
    runtime().block_on(async {
        let rmux = sdk(&harness).await?;
        let bytes = harness.tmpdir().join("input-bytes");
        let workload = format!(
            "stty raw -echo; printf INPUT-READY; dd bs=1 count=3 of='{}' 2>/dev/null; sleep 120",
            bytes.display()
        );
        cmd(&rmux, &["new-session", "-d", "-s", "alpha", &workload]).await?;
        let named = rmux
            .pane(PaneRef::new(SessionName::new("alpha")?, 0, 0))
            .await?;
        let pane = rmux
            .pane_by_id(
                SessionName::new("alpha")?,
                named.id().await?.expect("pane ID"),
            )
            .await?;
        wait_text(&pane, "INPUT-READY").await?;
        pane.send_text("A").await?;
        pane.send_key("Enter").await?;
        pane.send_text("B").await?;
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Ok(actual) = std::fs::read(&bytes) {
                if actual.len() >= 3 {
                    assert_eq!(actual, b"A\rB");
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "awaited input did not reach its pane"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let result = rmux
            .cmd([
                "set-option",
                "-t",
                "alpha",
                "@order",
                "first",
                ";",
                "set-option",
                "-t",
                "alpha",
                "@order",
                "second",
            ])
            .await?;
        assert_eq!(result.exit, Some(0));
        assert_eq!(
            cmd(&rmux, &["show-options", "-v", "-t", "alpha", "@order"])
                .await?
                .trim(),
            "second"
        );
        let failed = rmux
            .cmd([
                "select-pane",
                "-t",
                "%999999",
                ";",
                "set-option",
                "-t",
                "alpha",
                "@order",
                "forbidden",
            ])
            .await?;
        assert_eq!(failed.exit, Some(1));
        assert_eq!(
            cmd(&rmux, &["show-options", "-v", "-t", "alpha", "@order"])
                .await?
                .trim(),
            "second"
        );
        Ok(())
    })
}
