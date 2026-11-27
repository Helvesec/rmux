//! `#{buffer_full}` describes the buffer of the line it is rendered on.
//!
//! Every expectation here was measured on the pinned tmux 3.7b before a line of
//! this module existed, with the same commands: `cmd-list-buffers.c:69-82` binds
//! each buffer into its own format tree before the filter and before the
//! template, `format.c:5984-5986` is that binding, and `format.c:1429-1442`
//! reads the content of exactly that buffer. So two named buffers list as
//! `left:1:A` and `right:2:BB`, a filter on the content keeps only the buffer
//! that has it, and an automatic buffer joining them changes none of that.
//!
//! The preview is a different question and keeps its own rule here: these
//! witnesses only ask that the full text is the whole content and is not the
//! preview.

use rmux_core::command_parser::CommandParser;
use rmux_proto::{ListBuffersRequest, Request, Response};

use crate::handler::RequestHandler;

/// A text past every preview rule, with a tail nothing shorter can carry.
const LONG_TEXT: &str = "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789-BCTX1-TAIL";

/// The command queue, the entry `list-buffers` really arrives through.
async fn queue(handler: &RequestHandler, command: &str) -> Vec<u8> {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new()
                .parse(command)
                .expect("the command parses"),
        )
        .await
        .expect("the command succeeds")
        .stdout()
        .to_vec()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("stdout is utf-8")
}

/// The two named buffers the parcours starts from.
async fn two_named_buffers() -> RequestHandler {
    let handler = RequestHandler::new();
    queue(&handler, "set-buffer -b left A").await;
    queue(&handler, "set-buffer -b right BB").await;
    handler
}

const LIST: &str = "list-buffers -O name -F '#{buffer_name}:#{buffer_size}:#{buffer_full}'";

#[tokio::test]
async fn each_listed_line_carries_its_own_content() {
    let handler = two_named_buffers().await;

    assert_eq!(
        text(&queue(&handler, LIST).await),
        "left:1:A\nright:2:BB\n",
        "each line answers for the buffer it describes"
    );

    // The same lines through the request the client sends directly.
    let response = handler
        .handle(Request::ListBuffers(ListBuffersRequest {
            format: Some("#{buffer_name}:#{buffer_size}:#{buffer_full}".to_owned()),
            filter: None,
            sort_order: Some("name".to_owned()),
            reversed: false,
        }))
        .await;
    let Response::ListBuffers(listed) = response else {
        panic!("list-buffers answers a list");
    };
    assert_eq!(
        text(listed.output.stdout()),
        "left:1:A\nright:2:BB\n",
        "the direct request renders the same lines as the queued command"
    );
}

#[tokio::test]
async fn the_filter_reads_the_line_s_own_content() {
    let handler = two_named_buffers().await;

    assert_eq!(
        text(
            &queue(
                &handler,
                "list-buffers -O name -F '#{buffer_name}' -f '#{==:#{buffer_full},BB}'"
            )
            .await
        ),
        "right\n",
        "the filter keeps the buffer whose own content matches"
    );
    assert_eq!(
        text(
            &queue(
                &handler,
                "list-buffers -O name -F '#{buffer_name}' -f '#{==:#{buffer_full},NOPE}'"
            )
            .await
        ),
        "",
        "and keeps nothing when no buffer carries that content"
    );
}

#[tokio::test]
async fn an_automatic_buffer_does_not_capture_the_listed_lines() {
    let handler = two_named_buffers().await;
    // An automatic buffer is the head the global reading falls back to. It must
    // change nothing a line says about itself.
    queue(&handler, "set-buffer Z").await;

    assert_eq!(
        text(&queue(&handler, LIST).await),
        "buffer0:1:Z\nleft:1:A\nright:2:BB\n",
        "the automatic buffer takes its own line and leaves the others alone"
    );
    assert_eq!(
        text(
            &queue(
                &handler,
                "list-buffers -O name -F '#{buffer_name}' -f '#{==:#{buffer_full},BB}'"
            )
            .await
        ),
        "right\n",
        "the filter still selects on each line's own content"
    );
    assert_eq!(
        text(&queue(&handler, "show-buffer -b right").await),
        "BB",
        "and the buffer the filter found still gives its own bytes"
    );
}

#[tokio::test]
async fn the_full_text_is_not_the_preview() {
    let handler = RequestHandler::new();
    queue(&handler, &format!("set-buffer -b long {LONG_TEXT}")).await;

    assert_eq!(
        text(
            &queue(
                &handler,
                "list-buffers -F '#{buffer_size}|#{buffer_full}' -f '#{==:#{buffer_name},long}'"
            )
            .await
        ),
        format!("261|{LONG_TEXT}\n"),
        "the whole text comes back, tail included"
    );
    assert_eq!(
        text(&queue(
            &handler,
            "list-buffers -F '#{==:#{buffer_sample},#{buffer_full}}' -f '#{==:#{buffer_name},long}'"
        )
        .await),
        "0\n",
        "the full text is not the preview"
    );
}

#[tokio::test]
async fn the_store_is_empty_after_deletion() {
    let handler = two_named_buffers().await;
    queue(&handler, "set-buffer Z").await;

    for command in [
        "delete-buffer -b left",
        "delete-buffer -b right",
        "delete-buffer",
    ] {
        queue(&handler, command).await;
    }

    assert_eq!(
        text(&queue(&handler, LIST).await),
        "",
        "the store answers nothing once every buffer is gone"
    );
}

#[tokio::test]
async fn pane_path_without_pane_does_not_fall_through_to_global_environment() {
    let handler = RequestHandler::new();
    queue(&handler, "set-buffer -b witness A").await;
    queue(&handler, "set-environment -g pane_path spoof").await;
    queue(&handler, "set-environment -g PATH_SENTINEL ordinary").await;

    let queued = [
        text(&queue(&handler, "list-buffers -F '[#{pane_path}]'").await),
        text(
            &queue(
                &handler,
                "list-buffers -f '#{pane_path}' -F '#{buffer_name}'",
            )
            .await,
        ),
        text(&queue(&handler, "list-buffers -F '[#{PATH_SENTINEL}]'").await),
    ];
    let mut direct = Vec::new();
    for (format, filter) in [
        ("[#{pane_path}]", None),
        ("#{buffer_name}", Some("#{pane_path}")),
        ("[#{PATH_SENTINEL}]", None),
    ] {
        let response = handler
            .handle(Request::ListBuffers(ListBuffersRequest {
                format: Some(format.to_owned()),
                filter: filter.map(str::to_owned),
                sort_order: None,
                reversed: false,
            }))
            .await;
        let Response::ListBuffers(listed) = response else {
            panic!("list-buffers answers a list");
        };
        direct.push(text(listed.output.stdout()));
    }

    // A registered callback with no pane is terminal; ordinary environment
    // names still resolve. Both request paths use the buffer's format tree.
    let expected = ["[]\n", "", "[ordinary]\n"];
    assert_eq!(queued, expected);
    assert_eq!(direct, expected);
}
