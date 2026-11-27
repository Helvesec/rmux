use super::*;

// R2-271's two -b scenes differ: a real background shell loses its command
// client, while format evaluation keeps the foreground refusal and group scope.
#[tokio::test]
async fn deferred_attach_background_shell_and_format_keep_measured_client_scope() {
    for (scene, command, before, after, parent, expected_after, refuses) in [
        (
            "sibling-shell-if-b",
            "if-shell -b 'exit 0' { set-buffer -b sibling-sh-b-child-before YES ; attach-session -t deferred-alpha ; set-buffer -b sibling-sh-b-child-after YES } ; set-buffer -b sibling-sh-b-parent-after YES",
            "sibling-sh-b-child-before",
            "sibling-sh-b-child-after",
            "sibling-sh-b-parent-after",
            Some(b"YES".as_slice()),
            false,
        ),
        (
            "sibling-format-b-local",
            "if-shell -b -F 1 { set-buffer -b sibling-format-b-before YES ; attach-session -t deferred-alpha ; set-buffer -b sibling-format-b-after BAD } ; set-buffer -b sibling-format-b-parent YES",
            "sibling-format-b-before",
            "sibling-format-b-after",
            "sibling-format-b-parent",
            None,
            true,
        ),
    ] {
        let handler = RequestHandler::new();
        create_named_session(&handler, "deferred-alpha").await;
        let response = run_nested_commands(&handler, command.to_owned(), false, None).await;
        if refuses {
            assert!(matches!(
                &response,
                Response::Error(ErrorResponse { error: RmuxError::Server(message) })
                    if message == "open terminal failed: not a terminal"
            ), "{scene}: {response:?}");
        } else {
            assert!(matches!(response, Response::RunShell(_)), "{scene}: {response:?}");
        }
        wait_for_named_buffer(&handler, parent, b"YES").await;
        wait_for_named_buffer(&handler, before, b"YES").await;
        wait_for_detached_request_count(&handler, 0).await;
        assert_named_buffer(&handler, after, expected_after).await;
    }
}
