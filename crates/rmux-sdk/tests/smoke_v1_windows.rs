#[cfg(windows)]
mod common;

#[cfg(windows)]
mod windows {
    use super::common;

    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    use common::windows_smoke::{
        cmd_echo_text, cmd_interactive_command, session_name, wait_for_daemon_unavailable,
        wait_for_output_marker, Harness, TestResult, DEFAULT_TIMEOUT, LIVE_DAEMON_LOCK,
        OUTPUT_BUDGET,
    };
    use rmux_sdk::{
        EnsureSession, EnsureSessionPolicy, PaneOutputChunk, PaneOutputStart, PaneOutputStream,
        PaneProcessState,
    };
    use tokio::time::{sleep, timeout, Instant};

    const MARKER: &str = "RMUX_SDK_SMOKE_V1_WINDOWS_OK";

    #[tokio::test]
    async fn sdk_cmd_can_cold_start_an_independent_windows_daemon() -> TestResult {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let session_name = session_name("sdkwincmdcold");
        let harness = Harness::start_via_cmd("cmdcold", &session_name).await?;
        let pipe_name = harness.pipe_name().to_owned();

        assert!(
            harness
                .rmux()
                .list_sessions()
                .await?
                .iter()
                .any(|session| session == &session_name),
            "the command-started daemon must retain the requested session"
        );

        harness.finish().await?;
        wait_for_daemon_unavailable(&pipe_name).await?;
        Ok(())
    }

    #[tokio::test]
    async fn sdk_autostart_loads_default_config_from_the_exact_windows_caller_cwd() -> TestResult {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let root = windows_config_root()?;
        let cleanup = TempRoot::new(root.clone());
        let appdata = root.join("app data-é");
        let caller_cwd = root.join("caller dir-é");
        let config_path = appdata.join("rmux/rmux.conf");
        let relative_path = caller_cwd.join("sdk-relative.conf");
        let sentinel = session_name("sdkwinconfig");

        fs::create_dir_all(config_path.parent().expect("config path has parent"))?;
        fs::create_dir_all(&caller_cwd)?;
        fs::write(&config_path, b"source-file sdk-relative.conf\n")?;
        fs::write(
            &relative_path,
            format!("new-session -d -s {}\n", sentinel.as_str()),
        )?;

        let harness =
            Harness::start_with_default_config_environment("configcwd", &appdata, &caller_cwd)
                .await?;
        let first_sessions = harness.rmux().list_sessions().await?;
        assert!(
            first_sessions.iter().any(|session| session == &sentinel),
            "the first SDK request after connect_or_start must observe the config-created session"
        );

        harness.finish().await?;
        fs::remove_dir_all(&root)?;
        cleanup.disarm();
        Ok(())
    }

    #[tokio::test]
    async fn daemon_backed_sdk_windows_happy_path_uses_named_pipe_and_cleans_daemon() -> TestResult
    {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let harness = Harness::start("fresh").await?;
        let pipe_name = harness.pipe_name().to_owned();
        let rmux = harness.rmux();
        let session_name = session_name("sdkwinfresh");

        let warm = common::windows_smoke::builder(&pipe_name)
            .connect_or_start()
            .await?;
        assert!(
            warm.list_sessions().await?.is_empty(),
            "fresh Windows smoke daemon should start without preexisting sessions"
        );
        drop(warm);

        let session = rmux
            .ensure_session(
                EnsureSession::named(session_name.clone())
                    .policy(EnsureSessionPolicy::CreateOrReuse)
                    .detached(true)
                    .command(cmd_interactive_command()),
            )
            .await?;
        assert!(session.exists().await?);
        assert!(session.is_listed().await?);

        let pane = session.pane(0, 0);
        let recovery = pane.recover_output().await?;
        assert!(recovery.cols > 0 && recovery.rows > 0);
        assert_eq!(
            (recovery.snapshot.cols, recovery.snapshot.rows),
            (recovery.cols, recovery.rows)
        );
        recovery.snapshot.validate_shape()?;
        assert!(recovery.keyframe.starts_with(b"\x1b[?2026l"));
        let mut output = recovery.output;
        pane.send_text(cmd_echo_text(MARKER)).await?;
        wait_for_output_marker(&mut output, MARKER.as_bytes()).await?;
        drop(output);
        pane.wait_for_text(MARKER).await?;
        assert!(pane.snapshot().await?.visible_text().contains(MARKER));

        let recovery_after_output = pane.recover_output().await?;
        assert!(recovery_after_output.next_sequence > recovery.next_sequence);
        drop(recovery_after_output.output);
        exercise_atomic_recovery_after_lag(&pane).await?;

        harness.finish().await?;
        wait_for_daemon_unavailable(&pipe_name).await?;
        Ok(())
    }

    #[tokio::test]
    async fn detached_default_session_remains_sdk_ready_while_initial_pane_is_deferred(
    ) -> TestResult {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let harness = Harness::start("deferreddefault").await?;
        let rmux = harness.rmux();
        let session_name = session_name("sdkwindeferreddefault");

        let session = rmux
            .ensure_session(
                EnsureSession::named(session_name)
                    .policy(EnsureSessionPolicy::CreateOnly)
                    .detached(true),
            )
            .await?;
        assert!(session.exists().await?);
        assert!(session.is_listed().await?);

        let pane = session.pane(0, 0);
        let pane_id = pane.id().await?;
        assert!(
            pane_id.is_some(),
            "deferred pane should be listed immediately"
        );

        let armed_marker = "RMUX_SDK_DEFERRED_DEFAULT_ARMED_WAIT_OK";
        let armed_wait = pane.wait_for_text_next(armed_marker).await?;
        let mut output = pane.output_stream_starting_at(PaneOutputStart::Now).await?;
        pane.send_text(cmd_echo_text(armed_marker)).await?;
        armed_wait.await?;
        wait_for_output_marker(&mut output, armed_marker.as_bytes()).await?;

        let marker = "RMUX_SDK_DEFERRED_DEFAULT_WINDOWS_OK";
        pane.send_text(cmd_echo_text(marker)).await?;
        wait_for_output_marker(&mut output, marker.as_bytes()).await?;
        drop(output);

        pane.wait_for_text(marker).await?;
        assert!(pane.snapshot().await?.visible_text().contains(marker));

        wait_for_running_pane(&pane, "after SDK info sync").await?;

        harness.finish().await
    }

    #[tokio::test]
    async fn deferred_default_flushes_queued_input_before_live_sdk_input() -> TestResult {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let harness = Harness::start("deferredinputorder").await?;
        let rmux = harness.rmux();
        let session_name = session_name("sdkwindeferredinputorder");

        let session = rmux
            .ensure_session(
                EnsureSession::named(session_name)
                    .policy(EnsureSessionPolicy::CreateOnly)
                    .detached(true),
            )
            .await?;
        let pane = session.pane(0, 0);
        let mut output = pane.output_stream_starting_at(PaneOutputStart::Now).await?;

        let first_marker = "RMUX_SDK_DEFERRED_INPUT_FIRST_OK";
        let second_marker = "RMUX_SDK_DEFERRED_INPUT_SECOND_OK";
        let first_marker_input = windows_marker_output_text(first_marker);
        let second_marker_input = windows_marker_output_text(second_marker);
        assert!(!first_marker_input.contains(first_marker));
        assert!(!second_marker_input.contains(second_marker));
        let mut first_input = String::new();
        for index in 0..32 {
            first_input.push_str(&cmd_echo_text(&format!(
                "RMUX_SDK_DEFERRED_INPUT_PAD_{index}"
            )));
        }
        first_input.push_str(&first_marker_input);

        pane.send_text(first_input).await?;
        wait_for_running_pane(&pane, "after queued input flush").await?;

        pane.send_text(second_marker_input).await?;
        wait_for_markers_in_order(&mut output, first_marker, second_marker).await?;
        drop(output);

        harness.finish().await
    }

    async fn wait_for_markers_in_order(
        output: &mut PaneOutputStream,
        first: &str,
        second: &str,
    ) -> TestResult {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        let mut bytes = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("pane output did not contain {first:?} and {second:?}").into());
            }
            match timeout(remaining, output.next()).await?? {
                Some(PaneOutputChunk::Bytes { bytes: chunk, .. }) => {
                    bytes.extend_from_slice(&chunk);
                    if bytes.len() > OUTPUT_BUDGET {
                        let overflow = bytes.len() - OUTPUT_BUDGET;
                        bytes.drain(..overflow);
                    }
                    let text = String::from_utf8_lossy(&bytes);
                    if let (Some(first_pos), Some(second_pos)) =
                        (text.find(first), text.find(second))
                    {
                        assert!(
                            first_pos < second_pos,
                            "deferred queued input must be observed before later live input: {text:?}"
                        );
                        return Ok(());
                    }
                }
                Some(_) => {}
                None => return Err("pane output stream closed before markers appeared".into()),
            }
        }
    }

    async fn wait_for_running_pane(pane: &rmux_sdk::Pane, context: &str) -> TestResult {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            let info = pane.info().await?;
            let process = info
                .panes
                .first()
                .map(|pane| &pane.process)
                .expect("deferred pane should remain visible in SDK info");
            if matches!(process, PaneProcessState::Running { pid: Some(_) }) {
                return Ok(());
            }
            if matches!(process, PaneProcessState::Exited) {
                return Err(format!(
                    "deferred pane exited before publishing a running pid {context}"
                )
                .into());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "deferred pane did not publish a running pid {context}; last state {process:?}"
                )
                .into());
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn exercise_atomic_recovery_after_lag(pane: &rmux_sdk::Pane) -> TestResult {
        const FIRST: &str = "RMUX_SDK_LAG_BURST_ONE";
        const SECOND: &str = "RMUX_SDK_LAG_BURST_TWO";
        const AFTER: &str = "RMUX_SDK_POST_LAG_RECOVERY";

        let initial = pane.recover_output().await?;
        let initial_sequence = initial.next_sequence;
        let mut stale = initial.output;
        pane.send_text(windows_lag_burst_command("A", "BURST_ONE"))
            .await?;
        pane.wait_for_text(FIRST).await?;
        pane.send_text(windows_lag_burst_command("B", "BURST_TWO"))
            .await?;
        pane.wait_for_text(SECOND).await?;

        let Some(PaneOutputChunk::Lag(lag)) = timeout(DEFAULT_TIMEOUT, stale.next()).await?? else {
            return Err("stale recovery stream did not report bounded lag".into());
        };
        assert!(lag.missed_events > 0);
        drop(stale);

        let recovered = pane.recover_output().await?;
        assert!(recovered.next_sequence > initial_sequence);
        assert!(recovered.snapshot.visible_text().contains(SECOND));
        let mut output = recovered.output;
        pane.send_text(
            "powershell.exe -NoProfile -Command \"[Console]::Out.WriteLine(('RMUX_SDK_POST_' + 'LAG_RECOVERY'))\"\r",
        )
        .await?;
        wait_for_output_marker(&mut output, AFTER.as_bytes()).await?;
        Ok(())
    }

    fn windows_lag_burst_command(fill: &str, suffix: &str) -> String {
        format!(
            "powershell.exe -NoProfile -Command \"[Console]::Out.Write(('{fill}' * 180000)); [Console]::Out.WriteLine(); [Console]::Out.WriteLine(('RMUX_SDK_LAG_' + '{suffix}'))\"\r"
        )
    }

    fn windows_marker_output_text(text: &str) -> String {
        let codepoints = text
            .encode_utf16()
            .map(|codepoint| codepoint.to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "powershell.exe -NoProfile -Command \"Write-Output ([string]::Concat([char[]]@({codepoints})))\"\r"
        )
    }

    fn windows_config_root() -> TestResult<PathBuf> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(std::env::temp_dir().join(format!(
            "rmux-sdk-windows-config-{}-{nonce}",
            std::process::id()
        )))
    }

    struct TempRoot {
        path: PathBuf,
        armed: bool,
    }

    impl TempRoot {
        fn new(path: PathBuf) -> Self {
            Self { path, armed: true }
        }

        fn disarm(mut self) {
            self.armed = false;
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            if self.armed {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }
}

#[cfg(not(windows))]
#[test]
fn windows_smoke_tests_are_windows_only() {}
