//! End-to-end coverage for issue #85 on Windows: pane foreground state must
//! be observable through the `state_events` stream (`include_foreground`)
//! and through the revision-carrying snapshot query, end to end across the
//! daemon transport.
//!
//! Windows contract: the probe reports the pane's ROOT process (there is no
//! tcgetpgrp equivalent), labeling fields with the `RootProcess` /
//! `RuntimeName` sources, so a child launched inside the shell does not
//! change the reported foreground. The ForegroundChanged-on-child flow is
//! covered by the Unix twin in `pane_foreground_events.rs`.

#[cfg(windows)]
mod common;

#[cfg(windows)]
mod windows {
    use super::common;

    use std::time::Duration;

    use common::windows_smoke::{
        cmd_interactive_command, session_name, Harness, TestResult, LIVE_DAEMON_LOCK,
    };
    use rmux_sdk::{EnsureSession, PaneStateEvent, PaneStateEventsOptions};
    use tokio::time::timeout;

    const FOREGROUND_TIMEOUT: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn foreground_snapshot_and_revision_query_agree_end_to_end() -> TestResult {
        let _lock = LIVE_DAEMON_LOCK.lock().await;
        let harness = Harness::start("fgevents").await?;
        let rmux = harness.rmux();
        let name = session_name("sdkwinfg");

        let session = rmux
            .ensure_session(
                EnsureSession::named(name.clone())
                    .create_only()
                    .detached(true)
                    .command(cmd_interactive_command()),
            )
            .await?;
        let pane = session.pane(0, 0);

        let mut stream = pane
            .state_events(PaneStateEventsOptions {
                include_foreground: true,
                ..PaneStateEventsOptions::default()
            })
            .await?;
        let first = timeout(FOREGROUND_TIMEOUT, stream.next())
            .await
            .map_err(|_| "timed out waiting for the initial pane-state snapshot")??;
        let Some(PaneStateEvent::Snapshot {
            foreground,
            revision,
            ..
        }) = first
        else {
            return Err(format!("stream must open with a snapshot: {first:?}").into());
        };
        let foreground = foreground
            .ok_or("include_foreground snapshot must carry a foreground state on Windows")?;
        let snapshot_command = foreground.command.clone().unwrap_or_default();
        assert!(
            snapshot_command.to_ascii_lowercase().contains("cmd"),
            "Windows probe reports the pane root process: {foreground:?}"
        );

        // The revision-carrying query agrees with the stream and is orderable
        // against it.
        let (pane_id, query_revision, state) = pane
            .foreground_state_with_revision()
            .await?
            .ok_or("live pane must report a foreground snapshot")?;
        assert_eq!(Some(pane_id), pane.id().await?, "stable pane id matches");
        assert!(
            query_revision >= revision,
            "query revision {query_revision} must not precede the snapshot revision {revision}"
        );
        assert_eq!(
            state
                .command
                .clone()
                .unwrap_or_default()
                .to_ascii_lowercase(),
            snapshot_command.to_ascii_lowercase(),
            "query and stream agree on the root-process command"
        );
        assert!(
            state.sources.command.is_some(),
            "best-effort fields carry their source labels: {:?}",
            state.sources
        );

        session.kill().await?;
        harness.finish().await
    }
}
