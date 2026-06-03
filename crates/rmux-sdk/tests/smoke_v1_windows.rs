#[cfg(windows)]
mod common;

#[cfg(windows)]
mod windows {
    use super::common;

    use common::windows_smoke::{
        cmd_echo_text, cmd_interactive_command, session_name, wait_for_daemon_unavailable,
        wait_for_output_marker, Harness, TestResult, LIVE_DAEMON_LOCK,
    };
    use rmux_sdk::{EnsureSession, EnsureSessionPolicy, PaneOutputStart};

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
}

#[cfg(not(windows))]
#[test]
fn windows_smoke_tests_are_windows_only() {}
