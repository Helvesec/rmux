use super::sdk::{runtime, sdk, SdkBinary};
use super::*;
use rmux_proto::{PaneOutputSubscriptionStart, PaneTargetRef, Response};
use rmux_sdk::{CleanupPolicy, LeaseState, SessionName};

#[test]
fn l05_1_cli_rename_cross_store_atomic_snapshot() -> Result<(), Box<dyn Error>> {
    for collision in [false, true] {
        let harness = CliHarness::new("cli-rekey-stores")?;
        let _daemon = harness.start_hidden_daemon()?;
        let _binary = SdkBinary::pin();
        assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep 120"])?);
        runtime().block_on(async {
            let rmux = sdk(&harness).await?;
            let old = SessionName::new("alpha")?;
            let renamed = SessionName::new("renamed")?;
            let mut owned = rmux
                .owned_session(old.clone())
                .cleanup_policy(CleanupPolicy::KillOnOwnerExit)
                .lease_ttl(Duration::from_millis(600))
                .await?;
            let pane = owned.session().pane(0, 0);
            let id = pane.id().await?.expect("original pane ID");
            let stable = rmux.pane_by_id(old.clone(), id).await?;
            let socket = harness.socket_path().to_owned();
            let target = PaneTargetRef::by_id(old.clone(), id);
            let (mut output, response) = tokio::task::spawn_blocking(move || {
                let mut output = rmux_client::connect(&socket)?;
                let response =
                    output.subscribe_pane_output_ref(target, PaneOutputSubscriptionStart::Now)?;
                Ok::<_, rmux_client::ClientError>((output, response))
            })
            .await??;
            let Response::SubscribePaneOutput(subscription) = response else {
                panic!("{response:?}");
            };
            // The observed window is current; the default silence-action=other
            // deliberately suppresses its hook even when the timer expires.
            let guarded_cli_started = Instant::now();
            let first_option = harness
                .run_async(&["set-option", "-t", "alpha", "silence-action", "any"])
                .await?;
            eprintln!(
                "leased guarded CLI: collision={collision}, elapsed={:?}, state={:?}",
                guarded_cli_started.elapsed(),
                owned.lease_state()
            );
            assert!(
                rmux.has_session(old.clone()).await?,
                "owner lease expired while the guarded CLI ran on current_thread"
            );
            assert_success(&first_option);
            assert_success(
                &harness
                    .run_async(&["set-window-option", "-t", "alpha:0", "monitor-silence", "1"])
                    .await?,
            );
            assert_success(
                &harness
                    .run_async(&[
                        "set-hook",
                        "-t",
                        "alpha",
                        "alert-silence",
                        "set-option @timer expired",
                    ])
                    .await?,
            );
            if collision {
                assert_success(
                    &harness
                        .run_async(&["new-session", "-d", "-s", "renamed", "sleep 120"])
                        .await?,
                );
            }
            let identity = common::stdout(
                &harness
                    .run_async(&[
                        "display-message",
                        "-p",
                        "-t",
                        "alpha",
                        "#{session_id}:#{pane_id}",
                    ])
                    .await?,
            );
            let rename = harness
                .run_async(&["rename-session", "-t", "alpha", "renamed"])
                .await?;
            assert_eq!(rename.status.success(), !collision);
            let current = if collision { &old } else { &renamed };
            if !collision {
                assert_success(
                    &harness
                        .run_async(&["new-session", "-d", "-s", "alpha", "sleep 120"])
                        .await?,
                );
            }
            assert_eq!(
                common::stdout(
                    &harness
                        .run_async(&[
                            "display-message",
                            "-p",
                            "-t",
                            current.as_str(),
                            "#{session_id}:#{pane_id}"
                        ])
                        .await?
                ),
                identity
            );
            stable.send_text("printf 'REKEY-SUBSCRIPTION\\n'\n").await?;
            let deadline = Instant::now() + PROCESS_TIMEOUT;
            let mut bytes = Vec::new();
            loop {
                let subscription_id = subscription.subscription_id;
                let (connection, response) = tokio::task::spawn_blocking(move || {
                    let response = output.pane_output_cursor(subscription_id, Some(64))?;
                    Ok::<_, rmux_client::ClientError>((output, response))
                })
                .await??;
                output = connection;
                let Response::PaneOutputCursor(cursor) = response else {
                    panic!("{response:?}");
                };
                assert_eq!(cursor.subscription_id, subscription.subscription_id);
                for event in cursor.events {
                    bytes.extend(event.bytes);
                }
                if String::from_utf8_lossy(&bytes).contains("REKEY-SUBSCRIPTION") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "renamed subscription lost output"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Multiple TTLs exercise the same daemon lease token across rename.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert_eq!(owned.lease_state(), LeaseState::Active);
            assert_eq!(stable.id().await?, Some(id));
            let deadline = Instant::now() + PROCESS_TIMEOUT;
            loop {
                let marker = harness
                    .run_async(&["show-options", "-qv", "-t", current.as_str(), "@timer"])
                    .await?;
                if common::stdout(&marker) == "expired\n" {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "silence timer did not follow session identity"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let other = if collision { "renamed" } else { "alpha" };
            assert_eq!(
                common::stdout(
                    &harness
                        .run_async(&["show-options", "-qv", "-t", other, "@timer"])
                        .await?
                ),
                ""
            );
            tokio::task::spawn_blocking(move || {
                output.unsubscribe_pane_output(subscription.subscription_id)
            })
            .await??;
            owned.cleanup().await?;
            assert!(!rmux.has_session(current.clone()).await?);
            assert_success(&harness.run_async(&["has-session", "-t", other]).await?);
            Ok::<_, Box<dyn Error>>(())
        })?;
    }
    Ok(())
}
