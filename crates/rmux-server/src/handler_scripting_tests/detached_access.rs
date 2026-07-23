use super::*;

use rmux_ipc::PeerIdentity;
use rmux_os::identity::UserIdentity;

use crate::server_access::{current_owner_uid, ServerAccessAdmission};

#[derive(Clone, Copy)]
enum CallbackPath {
    Direct,
    SourceFile,
    Hook,
}

#[derive(Clone, Copy)]
enum AccessMutation {
    ReadOnly,
    Revoke,
}

#[tokio::test]
async fn direct_callback_after_read_only_change_is_rejected() {
    assert_stale_callback_rejected(
        CallbackPath::Direct,
        AccessMutation::ReadOnly,
        "direct-read-only",
        21_001,
    )
    .await;
}

#[tokio::test]
async fn direct_callback_after_revocation_is_rejected() {
    assert_stale_callback_rejected(
        CallbackPath::Direct,
        AccessMutation::Revoke,
        "direct-revoke",
        21_002,
    )
    .await;
}

#[tokio::test]
async fn source_file_callback_after_read_only_change_is_rejected() {
    assert_stale_callback_rejected(
        CallbackPath::SourceFile,
        AccessMutation::ReadOnly,
        "source-read-only",
        21_003,
    )
    .await;
}

#[tokio::test]
async fn source_file_callback_after_revocation_is_rejected() {
    assert_stale_callback_rejected(
        CallbackPath::SourceFile,
        AccessMutation::Revoke,
        "source-revoke",
        21_004,
    )
    .await;
}

#[tokio::test]
async fn hook_callback_after_read_only_change_is_rejected() {
    assert_stale_callback_rejected(
        CallbackPath::Hook,
        AccessMutation::ReadOnly,
        "hook-read-only",
        21_005,
    )
    .await;
}

#[tokio::test]
async fn hook_callback_after_revocation_is_rejected() {
    assert_stale_callback_rejected(
        CallbackPath::Hook,
        AccessMutation::Revoke,
        "hook-revoke",
        21_006,
    )
    .await;
}

#[tokio::test]
async fn same_pid_different_identity_scopes_fail_closed_without_merging_write() {
    let handler = RequestHandler::new();
    let requester_pid = 821_010;
    let write_uid = synthetic_uid(21_010);
    let read_only_uid = synthetic_uid(21_011);
    let write = grant_admission(&handler, write_uid, AccessMode::ReadWrite);
    let read_only = grant_admission(&handler, read_only_uid, AccessMode::ReadOnly);

    let write_scope = handler.begin_detached_requester_access(requester_pid, write);
    assert!(handler.requester_can_write(requester_pid).await);
    let read_only_scope = handler.begin_detached_requester_access(requester_pid, read_only);
    assert!(
        !handler.requester_can_write(requester_pid).await,
        "different identities sharing a PID must be ambiguous, not write-capable"
    );

    drop(read_only_scope);
    assert!(handler.requester_can_write(requester_pid).await);
    drop(write_scope);
    assert!(!handler.requester_can_write(requester_pid).await);
}

#[tokio::test]
async fn parallel_identical_scopes_preserve_their_exact_admission() {
    let handler = RequestHandler::new();
    let requester_pid = 821_012;
    let uid = synthetic_uid(21_012);
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);

    let first = handler.begin_detached_requester_access(requester_pid, admission.clone());
    let second = handler.begin_detached_requester_access(requester_pid, admission);
    assert!(handler.requester_can_write(requester_pid).await);

    drop(first);
    assert!(handler.requester_can_write(requester_pid).await);
    drop(second);
    assert!(!handler.requester_can_write(requester_pid).await);
}

#[tokio::test]
async fn new_admission_after_read_only_change_remains_bounded_by_its_initial_mode() {
    let handler = RequestHandler::new();
    let requester_pid = 821_013;
    let uid = synthetic_uid(21_013);
    let original = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let original_scope = handler.begin_detached_requester_access(requester_pid, original);

    handler
        .set_test_access_mode_for_uid(uid, AccessMode::ReadOnly)
        .expect("test access downgrades");
    let read_only = admission_for_uid(&handler, uid);
    let read_only_scope = handler.begin_detached_requester_access(requester_pid, read_only);
    assert!(!handler.requester_can_write(requester_pid).await);

    handler
        .set_test_access_mode_for_uid(uid, AccessMode::ReadWrite)
        .expect("test access upgrades");
    drop(original_scope);
    assert!(
        !handler.requester_can_write(requester_pid).await,
        "a read-only admission must not widen after the store is upgraded"
    );

    drop(read_only_scope);
    let current = admission_for_uid(&handler, uid);
    let current_scope = handler.begin_detached_requester_access(requester_pid, current);
    assert!(handler.requester_can_write(requester_pid).await);
    drop(current_scope);
}

#[tokio::test]
async fn new_admission_after_revoke_is_not_merged_with_the_stale_epoch() {
    let handler = RequestHandler::new();
    let requester_pid = 821_014;
    let uid = synthetic_uid(21_014);
    let stale = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let stale_scope = handler.begin_detached_requester_access(requester_pid, stale);

    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    assert!(!handler.requester_can_write(requester_pid).await);
    let fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let fresh_scope = handler.begin_detached_requester_access(requester_pid, fresh);
    assert!(
        !handler.requester_can_write(requester_pid).await,
        "stale and fresh generations sharing a PID must fail closed"
    );

    drop(stale_scope);
    assert!(handler.requester_can_write(requester_pid).await);
    drop(fresh_scope);
}

#[tokio::test]
async fn background_prompt_keeps_valid_detached_origin_after_request_scope_ends() {
    let handler = RequestHandler::new();
    let requester_pid = 821_020;
    let uid = synthetic_uid(21_020);
    let buffer_name = "prompt-valid-origin";
    let _control_rx = create_callback_attach(&handler, requester_pid, "prompt-valid-origin").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);

    let prompt = CommandParser::new()
        .parse(&format!(
            "command-prompt -b -pvalue {{ set-buffer -b {buffer_name} accepted }}"
        ))
        .expect("prompt parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, prompt)
        .await
        .expect("prompt starts");
    drop(scope);
    assert_no_detached_scope(&handler, requester_pid);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"ok\r")
        .await
        .expect("prompt accepts input");
    wait_for_background_task(&handler, "rmux-prompt-finish").await;
    wait_for_named_buffer(&handler, buffer_name, b"accepted").await;
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn background_prompt_rejects_remove_regrant_epoch_even_with_attach_pid_collision() {
    let handler = RequestHandler::new();
    let requester_pid = 821_021;
    let uid = synthetic_uid(21_021);
    let buffer_name = "prompt-stale-origin";
    let _control_rx = create_callback_attach(&handler, requester_pid, "prompt-stale-origin").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);

    let prompt = CommandParser::new()
        .parse(&format!(
            "command-prompt -b -pvalue {{ set-buffer -b {buffer_name} forbidden }}"
        ))
        .expect("prompt parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, prompt)
        .await
        .expect("prompt starts");
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"ok\r")
        .await
        .expect("prompt input is consumed");
    wait_for_background_task(&handler, "rmux-prompt-finish").await;
    assert_buffer_absent(&handler, buffer_name).await;
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn incremental_prompt_revalidates_its_origin_on_every_dispatch() {
    let handler = RequestHandler::new();
    let requester_pid = 821_022;
    let uid = synthetic_uid(21_022);
    let buffer_name = "prompt-incremental-origin";
    let _control_rx =
        create_callback_attach(&handler, requester_pid, "prompt-incremental-origin").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);

    let prompt = CommandParser::new()
        .parse(&format!(
            "command-prompt -b -i -pvalue {{ set-buffer -b {buffer_name} '%%' }}"
        ))
        .expect("incremental prompt parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, prompt)
        .await
        .expect("incremental prompt starts");
    wait_for_background_task(&handler, "rmux-prompt-dispatch").await;
    let initial = show_buffer(&handler, buffer_name)
        .await
        .expect("initial incremental dispatch writes");
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("incremental input is consumed");
    wait_for_background_task(&handler, "rmux-prompt-dispatch").await;
    assert_eq!(show_buffer(&handler, buffer_name).await, Some(initial));
    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x1b")
        .await
        .expect("prompt cancels");
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn confirm_before_rejects_stale_origin_after_remove_regrant() {
    let handler = RequestHandler::new();
    let requester_pid = 821_023;
    let uid = synthetic_uid(21_023);
    let buffer_name = "confirm-stale-origin";
    let _control_rx = create_callback_attach(&handler, requester_pid, "confirm-stale-origin").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);

    let confirm = CommandParser::new()
        .parse(&format!(
            "confirm-before -b -pconfirm {{ set-buffer -b {buffer_name} forbidden }}"
        ))
        .expect("confirmation parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, confirm)
        .await
        .expect("confirmation starts");
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"y")
        .await
        .expect("confirmation input is consumed");
    wait_for_background_task(&handler, "rmux-prompt-finish").await;
    assert_buffer_absent(&handler, buffer_name).await;
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn root_menu_rejects_stale_origin_instead_of_borrowing_colliding_attach() {
    let handler = RequestHandler::new();
    let requester_pid = 821_024;
    let uid = synthetic_uid(21_024);
    let buffer_name = "menu-stale-origin";
    let _control_rx = create_callback_attach(&handler, requester_pid, "menu-stale-origin").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    let menu = CommandParser::new()
        .parse(&format!(
            "display-menu -T Menu First f {{ set-buffer -b {buffer_name} forbidden }}"
        ))
        .expect("menu parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, menu)
        .await
        .expect("menu opens");
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    let error = handler
        .handle_attached_live_input_for_test(requester_pid, b"f")
        .await
        .expect_err("stale menu action must fail closed");
    assert_eq!(error.to_string(), "server error: client is read-only");
    assert_buffer_absent(&handler, buffer_name).await;
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn popup_internal_menu_captures_interacting_attach_origin_and_target() {
    let handler = RequestHandler::new();
    let popup_requester_pid = 821_025;
    let attach_pid = 821_026;
    let uid = synthetic_uid(21_025);
    let alpha = SessionName::new("popup-menu-origin-alpha").expect("valid session");
    let beta = SessionName::new("popup-menu-origin-beta").expect("valid session");
    let _control_rx = create_callback_attach(&handler, attach_pid, alpha.as_str()).await;
    create_detached_callback_session(&handler, beta.clone()).await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(popup_requester_pid, admission);
    let popup = CommandParser::new()
        .parse(&format!(
            "display-popup -N -c {attach_pid} -t {beta}:0.0 -w 20 -h 6"
        ))
        .expect("popup parses");
    handler
        .execute_parsed_commands_for_test(popup_requester_pid, popup)
        .await
        .expect("popup opens");
    drop(scope);

    let rect = {
        let active_attach = handler.active_attach.lock().await;
        let Some(super::super::overlay_support::ClientOverlayState::Popup(popup)) =
            active_attach.by_pid[&attach_pid].overlay.as_ref()
        else {
            panic!("popup is active");
        };
        assert_eq!(popup.current_target.session_name(), &beta);
        popup.rect
    };
    handler
        .handle_attached_live_input_for_test(attach_pid, &sgr_mouse(2, rect.x, rect.y))
        .await
        .expect("popup menu opens");

    let active_attach = handler.active_attach.lock().await;
    let Some(super::super::overlay_support::ClientOverlayState::Popup(popup)) =
        active_attach.by_pid[&attach_pid].overlay.as_ref()
    else {
        panic!("popup remains active");
    };
    let menu = popup.nested_menu.as_ref().expect("nested menu is active");
    assert_eq!(menu.origin.requester_pid(), attach_pid);
    assert_eq!(menu.current_target.session_name(), &alpha);
}

#[tokio::test]
async fn display_panes_custom_action_rejects_stale_origin_with_attach_pid_collision() {
    let handler = RequestHandler::new();
    let requester_pid = 821_027;
    let uid = synthetic_uid(21_027);
    let session_name = SessionName::new("display-panes-stale-origin").expect("valid session name");
    let buffer_name = "display-panes-stale-origin";
    let _control_rx = create_callback_attach(&handler, requester_pid, session_name.as_str()).await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    let response = handler
        .dispatch(
            requester_pid,
            Request::DisplayPanes(Box::new(rmux_proto::DisplayPanesRequest {
                target: session_name,
                duration_ms: Some(5_000),
                non_blocking: true,
                no_command: false,
                template: Some(format!("set-buffer -b {buffer_name} forbidden")),
                target_client: None,
            })),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::DisplayPanes(_)),
        "{response:?}"
    );
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    let error = handler
        .handle_attached_live_input_for_test(requester_pid, b"0")
        .await
        .expect_err("stale display-panes action must fail closed");
    assert_eq!(error.to_string(), "server error: client is read-only");
    assert_buffer_absent(&handler, buffer_name).await;
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn display_panes_default_action_rejects_stale_origin() {
    let handler = RequestHandler::new();
    let requester_pid = 821_028;
    let uid = synthetic_uid(21_028);
    let session_name = SessionName::new("display-panes-default-stale").expect("valid session name");
    let _control_rx = create_callback_attach(&handler, requester_pid, session_name.as_str()).await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    let response = handler
        .dispatch(
            requester_pid,
            Request::DisplayPanes(Box::new(rmux_proto::DisplayPanesRequest {
                target: session_name,
                duration_ms: Some(5_000),
                non_blocking: true,
                no_command: false,
                template: None,
                target_client: None,
            })),
        )
        .await
        .response;
    assert!(
        matches!(response, Response::DisplayPanes(_)),
        "{response:?}"
    );
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    let error = handler
        .handle_attached_live_input_for_test(requester_pid, b"0")
        .await
        .expect_err("stale default display-panes action must fail closed");
    assert_eq!(error.to_string(), "server error: client is read-only");
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn mode_tree_default_action_rejects_stale_origin_before_buffer_mutation() {
    let handler = RequestHandler::new();
    let requester_pid = 821_029;
    let uid = synthetic_uid(21_029);
    let buffer_name = "mode-default-stale";
    let _control_rx = create_callback_attach(&handler, requester_pid, "mode-default-stale").await;
    set_named_buffer(&handler, buffer_name, b"preserved").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    open_mode_tree(&handler, requester_pid, "choose-buffer").await;
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    let error = handler
        .handle_attached_live_input_for_test(requester_pid, b"P")
        .await
        .expect_err("stale default mode-tree action must fail closed");
    assert_eq!(error.to_string(), "server error: client is read-only");
    assert_eq!(
        show_buffer(&handler, buffer_name).await,
        Some(b"preserved".to_vec())
    );
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn mode_tree_custom_template_rejects_stale_origin() {
    let handler = RequestHandler::new();
    let requester_pid = 821_030;
    let uid = synthetic_uid(21_030);
    let source_buffer = "mode-custom-source";
    let marker = "mode-custom-stale";
    let _control_rx = create_callback_attach(&handler, requester_pid, "mode-custom-stale").await;
    set_named_buffer(&handler, source_buffer, b"source").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    open_mode_tree(
        &handler,
        requester_pid,
        &format!("choose-buffer {{ set-buffer -b {marker} forbidden }}"),
    )
    .await;
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    let error = handler
        .handle_attached_live_input_for_test(requester_pid, b"\r")
        .await
        .expect_err("stale custom mode-tree action must fail closed");
    assert_eq!(error.to_string(), "server error: client is read-only");
    assert_buffer_absent(&handler, marker).await;
    assert_no_detached_scope(&handler, requester_pid);
}

#[tokio::test]
async fn mode_tree_command_prompt_preserves_stale_origin() {
    let handler = RequestHandler::new();
    let requester_pid = 821_031;
    let uid = synthetic_uid(21_031);
    let source_buffer = "mode-prompt-source";
    let marker = "mode-prompt-stale";
    let _control_rx = create_callback_attach(&handler, requester_pid, "mode-prompt-stale").await;
    set_named_buffer(&handler, source_buffer, b"source").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    open_mode_tree(&handler, requester_pid, "choose-buffer").await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b":")
        .await
        .expect("mode-tree command prompt opens");
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    handler
        .handle_attached_live_input_for_test(
            requester_pid,
            format!("set-buffer -b {marker} forbidden\r").as_bytes(),
        )
        .await
        .expect("mode-tree prompt input is consumed");
    wait_for_no_detached_scope(&handler, requester_pid).await;
    assert_buffer_absent(&handler, marker).await;
}

#[tokio::test]
async fn mode_tree_confirmation_preserves_stale_origin() {
    let handler = RequestHandler::new();
    let requester_pid = 821_032;
    let uid = synthetic_uid(21_032);
    let buffer_name = "mode-confirm-stale";
    let _control_rx = create_callback_attach(&handler, requester_pid, "mode-confirm-stale").await;
    set_named_buffer(&handler, buffer_name, b"preserved").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let scope = handler.begin_detached_requester_access(requester_pid, admission);
    open_mode_tree(&handler, requester_pid, "choose-buffer").await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"x")
        .await
        .expect("mode-tree confirmation opens");
    drop(scope);
    handler
        .remove_test_access_for_uid(uid)
        .expect("test access revokes");
    let _fresh = grant_admission(&handler, uid, AccessMode::ReadWrite);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"y")
        .await
        .expect("mode-tree confirmation input is consumed");
    wait_for_no_detached_scope(&handler, requester_pid).await;
    assert_eq!(
        show_buffer(&handler, buffer_name).await,
        Some(b"preserved".to_vec())
    );
}

#[tokio::test]
async fn detached_mode_tree_command_prompt_targets_interacting_attach() {
    let handler = RequestHandler::new();
    let requester_pid = 821_033;
    let first_attach_pid = 821_034;
    let interacting_attach_pid = 821_035;
    let uid = synthetic_uid(21_033);
    let session_name = SessionName::new("mode-prompt-multiple-attaches").expect("valid session");
    let _first_rx = create_callback_attach(&handler, first_attach_pid, session_name.as_str()).await;
    let _interacting_rx =
        register_callback_attach(&handler, interacting_attach_pid, session_name.clone()).await;
    set_named_buffer(&handler, "mode-prompt-multiple-source", b"source").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let _scope = handler.begin_detached_requester_access(requester_pid, admission);

    open_mode_tree_for_attach(
        &handler,
        requester_pid,
        interacting_attach_pid,
        "choose-buffer",
    )
    .await;
    handler
        .handle_attached_live_input_for_test(interacting_attach_pid, b":")
        .await
        .expect("mode-tree command prompt opens on the interacting attach");

    assert!(!handler.prompt_active(first_attach_pid).await);
    assert!(handler.prompt_active(interacting_attach_pid).await);
    handler
        .handle_attached_live_input_for_test(interacting_attach_pid, b"\x1b")
        .await
        .expect("mode-tree command prompt cancels");
}

#[tokio::test]
async fn detached_mode_tree_confirmation_targets_interacting_attach() {
    let handler = RequestHandler::new();
    let requester_pid = 821_036;
    let first_attach_pid = 821_037;
    let interacting_attach_pid = 821_038;
    let uid = synthetic_uid(21_036);
    let session_name = SessionName::new("mode-confirm-multiple-attaches").expect("valid session");
    let _first_rx = create_callback_attach(&handler, first_attach_pid, session_name.as_str()).await;
    let _interacting_rx =
        register_callback_attach(&handler, interacting_attach_pid, session_name.clone()).await;
    set_named_buffer(&handler, "mode-confirm-multiple-source", b"preserved").await;
    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let _scope = handler.begin_detached_requester_access(requester_pid, admission);

    open_mode_tree_for_attach(
        &handler,
        requester_pid,
        interacting_attach_pid,
        "choose-buffer",
    )
    .await;
    handler
        .handle_attached_live_input_for_test(interacting_attach_pid, b"x")
        .await
        .expect("mode-tree confirmation opens on the interacting attach");

    assert!(!handler.prompt_active(first_attach_pid).await);
    assert!(handler.prompt_active(interacting_attach_pid).await);
    handler
        .handle_attached_live_input_for_test(interacting_attach_pid, b"n")
        .await
        .expect("mode-tree confirmation declines");
}

async fn assert_stale_callback_rejected(
    path: CallbackPath,
    mutation: AccessMutation,
    label: &str,
    uid_offset: u32,
) {
    let handler = RequestHandler::new();
    let requester_pid = 800_000_u32.saturating_add(uid_offset);
    let uid = synthetic_uid(uid_offset);
    let buffer_name = format!("detached-{label}");

    if matches!(path, CallbackPath::Hook) {
        let setup = CommandParser::new()
            .parse(&format!(
                "set-hook -g after-display-message {{ set-buffer -b {buffer_name} forbidden }}"
            ))
            .expect("hook setup parses");
        handler
            .execute_parsed_commands_for_test(std::process::id(), setup)
            .await
            .expect("hook setup succeeds");
    }

    let admission = grant_admission(&handler, uid, AccessMode::ReadWrite);
    let request_scope = handler.begin_detached_requester_access(requester_pid, admission);
    let callback_scope = handler
        .begin_inherited_detached_requester_access(requester_pid)
        .await;
    drop(request_scope);

    match mutation {
        AccessMutation::ReadOnly => handler
            .set_test_access_mode_for_uid(uid, AccessMode::ReadOnly)
            .expect("test access downgrades like server-access -r"),
        AccessMutation::Revoke => handler
            .remove_test_access_for_uid(uid)
            .expect("test access revokes like server-access -d"),
    }

    run_callback(&handler, requester_pid, path, &buffer_name).await;
    assert!(matches!(
        handler
            .handle(Request::ShowBuffer(ShowBufferRequest {
                name: Some(buffer_name),
            }))
            .await,
        Response::Error(_)
    ));
    drop(callback_scope);
}

async fn run_callback(
    handler: &RequestHandler,
    requester_pid: u32,
    path: CallbackPath,
    buffer_name: &str,
) {
    match path {
        CallbackPath::Direct => {
            let commands = CommandParser::new()
                .parse(&format!("set-buffer -b {buffer_name} forbidden"))
                .expect("direct callback parses");
            let error = handler
                .execute_parsed_commands_for_test(requester_pid, commands)
                .await
                .expect_err("stale direct callback must be read-only");
            assert_eq!(error.to_string(), "server error: client is read-only");
        }
        CallbackPath::SourceFile => {
            let response = handler
                .dispatch(
                    requester_pid,
                    Request::SourceFile(Box::new(SourceFileRequest {
                        paths: vec!["-".to_owned()],
                        quiet: false,
                        parse_only: false,
                        verbose: false,
                        expand_paths: false,
                        target: None,
                        caller_cwd: None,
                        stdin: Some(format!("set-buffer -b {buffer_name} forbidden\n")),
                    })),
                )
                .await
                .response;
            let Response::SourceFile(response) = response else {
                panic!("stale source-file callback must return a source result");
            };
            assert_eq!(response.exit_status(), Some(1));
        }
        CallbackPath::Hook => {
            let commands = CommandParser::new()
                .parse("display-message -p callback")
                .expect("hook trigger parses");
            handler
                .execute_parsed_commands_for_test(requester_pid, commands)
                .await
                .expect("read-only hook trigger remains allowed");
        }
    }
}

fn synthetic_uid(offset: u32) -> u32 {
    current_owner_uid().wrapping_add(offset).max(1)
}

fn grant_admission(handler: &RequestHandler, uid: u32, mode: AccessMode) -> ServerAccessAdmission {
    handler
        .set_test_access_mode_for_uid(uid, mode)
        .expect("test access grants");
    admission_for_uid(handler, uid)
}

fn admission_for_uid(handler: &RequestHandler, uid: u32) -> ServerAccessAdmission {
    handler
        .server_access_admission_for_peer(&PeerIdentity {
            pid: 0,
            uid,
            user: UserIdentity::Uid(uid),
        })
        .expect("granted test peer has an admission")
}

async fn create_callback_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    name: &str,
) -> tokio::sync::mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let session_name = SessionName::new(name).expect("valid callback session name");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    register_callback_attach(handler, requester_pid, session_name).await
}

async fn register_callback_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session_name: SessionName,
) -> tokio::sync::mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session_name, control_tx)
        .await;
    control_rx
}

async fn create_detached_callback_session(handler: &RequestHandler, session_name: SessionName) {
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
}

async fn set_named_buffer(handler: &RequestHandler, name: &str, content: &[u8]) {
    let response = handler
        .handle(Request::SetBuffer(Box::new(rmux_proto::SetBufferRequest {
            name: Some(name.to_owned()),
            content: content.to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        })))
        .await;
    assert!(matches!(response, Response::SetBuffer(_)), "{response:?}");
}

async fn open_mode_tree(handler: &RequestHandler, requester_pid: u32, command: &str) {
    open_mode_tree_for_attach(handler, requester_pid, requester_pid, command).await;
}

async fn open_mode_tree_for_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    attach_pid: u32,
    command: &str,
) {
    let parsed = CommandParser::new()
        .parse(command)
        .expect("mode-tree command parses");
    handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("mode-tree opens");
    assert!(handler.mode_tree_active(attach_pid).await);
}

fn sgr_mouse(button: u16, x: u16, y: u16) -> Vec<u8> {
    format!(
        "\x1b[<{button};{};{}M",
        x.saturating_add(1),
        y.saturating_add(1)
    )
    .into_bytes()
}

async fn wait_for_background_task(handler: &RequestHandler, name: &'static str) {
    tokio::task::yield_now().await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while handler.background_task_running_for_test(name) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("background task {name} did not finish"));
}

async fn wait_for_no_detached_scope(handler: &RequestHandler, requester_pid: u32) {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !handler
                .active_detached_requester_access
                .lock()
                .expect("detached requester access lock")
                .contains_key(&requester_pid)
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("callback origin guard clears");
}

async fn show_buffer(handler: &RequestHandler, name: &str) -> Option<Vec<u8>> {
    handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some(name.to_owned()),
        }))
        .await
        .command_output()
        .map(|output| output.stdout().to_vec())
}

async fn assert_buffer_absent(handler: &RequestHandler, name: &str) {
    assert!(show_buffer(handler, name).await.is_none());
}

fn assert_no_detached_scope(handler: &RequestHandler, requester_pid: u32) {
    assert!(
        !handler
            .active_detached_requester_access
            .lock()
            .expect("detached requester access lock")
            .contains_key(&requester_pid),
        "callback origin guard must leave no residual detached scope"
    );
}
