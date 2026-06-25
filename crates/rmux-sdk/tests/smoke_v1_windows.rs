#[cfg(windows)]
mod common;

#[cfg(windows)]
mod windows {
    use super::common;

    use std::time::Duration;

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
        let mut output = pane.output_stream_starting_at(PaneOutputStart::Now).await?;
        pane.send_text(cmd_echo_text(MARKER)).await?;
        wait_for_output_marker(&mut output, MARKER.as_bytes()).await?;
        drop(output);
        pane.wait_for_text(MARKER).await?;
        assert!(pane.snapshot().await?.visible_text().contains(MARKER));

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
}

#[cfg(not(windows))]
#[test]
fn windows_smoke_tests_are_windows_only() {}
