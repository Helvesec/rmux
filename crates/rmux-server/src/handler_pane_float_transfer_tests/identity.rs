//! FMT1 R2 — the pane a queued transfer names must be the pane it moves.
//!
//! An ordinary scripting queue resolves `-s` and `-t` under the parsing lock,
//! then releases it and awaits before the mutation. These witnesses drive the
//! acquired pause seam, replace one slot during that wait, and require the
//! refusal; then they let an untouched transfer through to prove the guard is
//! not simply refusing everything.

use std::time::Duration;

use rmux_core::command_parser::CommandParser;
use rmux_proto::{KillPaneRequest, Request, RespawnPaneRequest, Response};

use super::*;
use crate::handler::scripting_support::{
    install_queue_exact_target_capture_pause, QueueExactTargetCapturePause,
};

async fn wait_for_pause(pause: &QueueExactTargetCapturePause) {
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("the queue reaches the capture pause");
}

/// Replaces the pane living at `pane_index` with a different identity at the
/// same slot: kill it, then split the survivor so the slot is filled again.
async fn replace_pane_slot(
    handler: &RequestHandler,
    name: &SessionName,
    window_index: u32,
    pane_index: u32,
    keep_pane_index: u32,
) -> PaneId {
    let response = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: pane(name, window_index, pane_index),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(response, Response::KillPane(_)), "{response:?}");
    queue(
        handler,
        &format!("split-window -d -h -t {name}:{window_index}.{keep_pane_index}"),
    )
    .await
    .expect("the slot is filled again");
    let current = window(handler, name, window_index).await;
    current
        .pane(pane_index)
        .unwrap_or_else(|| panic!("pane {pane_index} exists again"))
        .id()
}

#[tokio::test]
async fn a_queued_transfer_refuses_a_source_replaced_during_its_wait() {
    for command in ["move-pane", "join-pane"] {
        let label = format!("fmt1-id-src-{command}");
        let (handler, name, source_id, _witness_id, tile_id) = fixture(&label).await;
        // A second tile gives the kill a survivor to split, so the slot the
        // queue resolved is filled by a different pane identity.
        queue(&handler, &format!("split-window -d -h -t {name}:0.0"))
            .await
            .expect("second tile");
        let before = window(&handler, &name, 0).await;
        let source_index = index_of(&before, source_id);
        let tile_index = index_of(&before, tile_id);
        let parsed = CommandParser::new()
            .parse(&format!(
                "{command} -d -s {name}:0.{source_index} -t {name}:0.{tile_index}"
            ))
            .expect("command parses");
        let pause = install_queue_exact_target_capture_pause(&handler, command);
        let queued_handler = handler.clone();
        let queued = tokio::spawn(async move {
            queued_handler
                .execute_parsed_commands_for_test(std::process::id(), parsed)
                .await
        });

        wait_for_pause(&pause).await;
        let replacement_id = replace_pane_slot(&handler, &name, 0, source_index, tile_index).await;
        assert_ne!(
            replacement_id, source_id,
            "{label}: the slot really changed"
        );
        pause.release.notify_one();

        let error = tokio::time::timeout(Duration::from_secs(5), queued)
            .await
            .expect("the queued command finishes")
            .expect("the queued task joins")
            .expect_err("a replaced source must not be transferred");
        assert!(
            error.to_string().contains("target identity changed"),
            "{label}: {error}"
        );
        let after = window(&handler, &name, 0).await;
        assert!(
            after.floating_stack().contains(replacement_id) || !after.floating_stack().is_empty(),
            "{label}: the replacement stayed where it was"
        );
        assert_eq!(
            after
                .panes()
                .iter()
                .filter(|candidate| candidate.id() == replacement_id)
                .count(),
            1,
            "{label}: the replacement is still the pane at that slot"
        );
        cleanup(&handler).await;
    }
}

#[tokio::test]
async fn a_queued_transfer_refuses_a_destination_respawned_during_its_wait() {
    // The destination keeps its slot and its PaneId here, but a respawn gives
    // it a new runtime: the pane the queue resolved is not the pane that would
    // receive the transfer, and the guard has to say so for both roles.
    for command in ["move-pane", "join-pane"] {
        let label = format!("fmt1-id-dst-{command}");
        let (handler, name, source_id, _witness_id, tile_id) = fixture(&label).await;
        let before = window(&handler, &name, 0).await;
        let source_index = index_of(&before, source_id);
        let destination_index = index_of(&before, tile_id);
        let parsed = CommandParser::new()
            .parse(&format!(
                "{command} -d -s {name}:0.{source_index} -t {name}:0.{destination_index}"
            ))
            .expect("command parses");
        let pause = install_queue_exact_target_capture_pause(&handler, command);
        let queued_handler = handler.clone();
        let queued = tokio::spawn(async move {
            queued_handler
                .execute_parsed_commands_for_test(std::process::id(), parsed)
                .await
        });

        wait_for_pause(&pause).await;
        let respawned = handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: pane(&name, 0, destination_index),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec![crate::test_shell::stdin_discard_command()]),
                process_command: None,
            })))
            .await;
        assert!(
            matches!(respawned, Response::RespawnPane(_)),
            "{label}: {respawned:?}"
        );
        pause.release.notify_one();

        let error = tokio::time::timeout(Duration::from_secs(5), queued)
            .await
            .expect("the queued command finishes")
            .expect("the queued task joins")
            .expect_err("a respawned destination must not receive the pane");
        assert!(
            error.to_string().contains("target identity changed"),
            "{label}: {error}"
        );
        let after = window(&handler, &name, 0).await;
        assert!(
            after.floating_stack().contains(source_id),
            "{label}: the source is still floating where the queue found it"
        );
        assert_eq!(
            index_of(&after, tile_id),
            destination_index,
            "{label}: the destination kept its slot"
        );
        cleanup(&handler).await;
    }
}

#[tokio::test]
async fn an_untouched_queued_transfer_still_passes_through_the_same_pause() {
    // The guard must refuse a replacement, not every queued transfer: the same
    // seam, the same wait, nothing replaced.
    for command in ["move-pane", "join-pane"] {
        let label = format!("fmt1-id-ok-{command}");
        let (handler, name, source_id, witness_id, tile_id) = fixture(&label).await;
        let before = window(&handler, &name, 0).await;
        let source_index = index_of(&before, source_id);
        let tile_index = index_of(&before, tile_id);
        let parsed = CommandParser::new()
            .parse(&format!(
                "{command} -d -h -s {name}:0.{source_index} -t {name}:0.{tile_index}"
            ))
            .expect("command parses");
        let pause = install_queue_exact_target_capture_pause(&handler, command);
        let queued_handler = handler.clone();
        let queued = tokio::spawn(async move {
            queued_handler
                .execute_parsed_commands_for_test(std::process::id(), parsed)
                .await
        });

        wait_for_pause(&pause).await;
        pause.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), queued)
            .await
            .expect("the queued command finishes")
            .expect("the queued task joins")
            .unwrap_or_else(|error| panic!("{label}: an untouched transfer must pass: {error}"));

        let after = window(&handler, &name, 0).await;
        assert_eq!(
            pane_ids(&after),
            vec![(0, tile_id), (1, source_id), (2, witness_id)]
        );
        assert_eq!(
            geometry(&after, source_id),
            rmux_core::PaneGeometry::new(41, 0, 39, 24)
        );
        assert_eq!(stack_ids(&after), vec![witness_id]);
        cleanup(&handler).await;
    }
}

#[tokio::test]
async fn a_requester_that_loses_write_authority_is_still_refused_after_the_same_wait() {
    // Authority revalidation sits after the capture pause; the new identity
    // capture must not shadow it.
    use rmux_ipc::PeerIdentity;
    use rmux_os::identity::UserIdentity;

    use crate::server_access::{current_owner_uid, AccessMode};

    let (handler, name, source_id, _witness_id, tile_id) = fixture("fmt1-id-ro").await;
    let requester_pid = 821_501;
    let uid = current_owner_uid().wrapping_add(21_501).max(1);
    handler
        .set_test_access_mode_for_uid(uid, AccessMode::ReadWrite)
        .expect("test access grants");
    let admission = handler
        .server_access_admission_for_peer(&PeerIdentity {
            pid: 0,
            uid,
            user: UserIdentity::Uid(uid),
        })
        .expect("granted test peer has an admission");
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    assert!(handler.requester_can_write(requester_pid).await);

    let before = window(&handler, &name, 0).await;
    let source_index = index_of(&before, source_id);
    let tile_index = index_of(&before, tile_id);
    let parsed = CommandParser::new()
        .parse(&format!(
            "move-pane -d -h -s {name}:0.{source_index} -t {name}:0.{tile_index}"
        ))
        .expect("command parses");
    let pause = install_queue_exact_target_capture_pause(&handler, "move-pane");
    let queued_handler = handler.clone();
    let queued = tokio::spawn(async move {
        queued_handler
            .execute_parsed_commands_for_test(requester_pid, parsed)
            .await
    });

    wait_for_pause(&pause).await;
    drop(scope);
    assert!(!handler.requester_can_write(requester_pid).await);
    pause.release.notify_one();

    let error = tokio::time::timeout(Duration::from_secs(5), queued)
        .await
        .expect("the queued command finishes")
        .expect("the queued task joins")
        .expect_err("a requester without write authority must not transfer");
    assert!(error.to_string().contains("read-only"), "{error}");
    let after = window(&handler, &name, 0).await;
    assert!(
        after.floating_stack().contains(source_id),
        "the source never moved"
    );
    cleanup(&handler).await;
}
