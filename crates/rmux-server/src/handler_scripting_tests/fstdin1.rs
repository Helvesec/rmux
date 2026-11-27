//! FSTDIN1 — a finished tool's output displayed in a float.
//!
//! The expectations are literals measured on the pinned tmux 3.7b before any
//! line of this lot (`fstdin1/native/oracle-fstdin1.json`), in a
//! fixture whose window is 80x24 and whose float is the default 40x6.

use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;
use rmux_proto::{HandshakeRequest, NewPaneInputV1Request, NewPaneInputV1Response};
use std::time::Duration;

fn offer(arguments: &[&str], payload: Option<Vec<u8>>) -> NewPaneInputV1Request {
    NewPaneInputV1Request::new(
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        None,
        payload,
    )
}

async fn send(handler: &RequestHandler, offer: NewPaneInputV1Request) -> NewPaneInputV1Response {
    let Response::NewPaneInputV1(result) = handler
        .handle(Request::NewPaneInputV1(Box::new(offer)))
        .await
    else {
        panic!("new-pane input offer result")
    };
    *result
}

/// Opens a float on the finished bytes and returns the pane it printed.
async fn displayed(handler: &RequestHandler, payload: &[u8]) -> String {
    let result = send(
        handler,
        offer(
            &["new-pane", "-I", "-d", "-P", "-F", "#{pane_id}"],
            Some(payload.to_vec()),
        ),
    )
    .await;
    match result {
        NewPaneInputV1Response::Complete {
            stdout,
            error: None,
            exit_status: 0,
        } => String::from_utf8(stdout)
            .expect("the printed identity is utf-8")
            .trim_end()
            .to_owned(),
        other => panic!("{payload:?}: {other:?}"),
    }
}

async fn printed(handler: &RequestHandler, command: &str) -> String {
    let output = queue(handler, command)
        .await
        .unwrap_or_else(|error| panic!("{command}: {error}"));
    String::from_utf8(output.stdout().to_vec()).expect("printed text is utf-8")
}

/// What the float holds, as `capture-pane` prints it.
async fn content(handler: &RequestHandler, pane: &str) -> String {
    printed(handler, &format!("capture-pane -p -t {pane}")).await
}

/// Where the float's cursor is, and whether it owns a process.
async fn cursor(handler: &RequestHandler, pane: &str) -> String {
    printed(
        handler,
        &format!("display-message -p -t {pane} '#{{cursor_x}} #{{cursor_y}}'"),
    )
    .await
}

#[tokio::test]
async fn an_end_of_file_without_a_byte_opens_a_float_with_nothing_in_it() {
    let handler = setup().await;
    // Measured (cell n01): a producer that writes nothing and ends still opens
    // the float, which holds nothing and keeps its cursor at the origin. The
    // channel reached end of file; that is not the same as no channel at all.
    let pane = displayed(&handler, b"").await;
    assert_eq!(window(&handler).await.pane_count(), 2);
    // `capture-pane` prints the whole float, so the six rows of an empty one
    // are six empty lines, exactly as the pin printed them.
    assert_eq!(content(&handler, &pane).await, "\n\n\n\n\n\n");
    assert_eq!(cursor(&handler, &pane).await, "0 0\n");
    // The float has no process, and none was invented for it.
    assert_eq!(
        printed(
            &handler,
            &format!("display-message -p -t {pane} '#{{pane_pid}}|#{{pane_dead}}'")
        )
        .await,
        "0|0\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn bare_line_feeds_read_like_the_carriage_returned_with_them() {
    let handler = setup().await;
    // Measured (cells n02 and n03): tmux gives an empty pane a screen where a
    // line feed also returns the carriage, so these two finished outputs are
    // read the same way and leave the cursor in the same place. The bytes
    // themselves are never rewritten on the way in.
    let bare = displayed(&handler, b"alpha\nbeta\n").await;
    assert_eq!(content(&handler, &bare).await, "alpha\nbeta\n\n\n\n\n");
    assert_eq!(cursor(&handler, &bare).await, "0 2\n");
    let with_carriage = displayed(&handler, b"alpha\r\nbeta\r\n").await;
    assert_eq!(
        content(&handler, &with_carriage).await,
        "alpha\nbeta\n\n\n\n\n"
    );
    assert_eq!(cursor(&handler, &with_carriage).await, "0 2\n");
    // Measured (cell n08): a carriage return inside the payload still returns
    // the carriage, so what follows it overwrites what was already there.
    let overwritten = displayed(&handler, b"abc\rX\n").await;
    assert_eq!(content(&handler, &overwritten).await, "Xbc\n\n\n\n\n\n");
    assert_eq!(cursor(&handler, &overwritten).await, "0 1\n");
    cleanup(&handler).await;
}

#[tokio::test]
async fn output_that_reads_like_a_command_is_only_ever_content() {
    let handler = setup().await;
    // Measured (cell n04): the text is displayed and nothing of it is executed
    // or sourced; the server is still there afterwards.
    let pane = displayed(&handler, b"kill-server\n").await;
    assert_eq!(content(&handler, &pane).await, "kill-server\n\n\n\n\n\n");
    assert_eq!(window(&handler).await.pane_count(), 2);
    assert_eq!(
        printed(&handler, "display-message -p 'alive'").await,
        "alive\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_command_beside_the_input_is_refused_and_creates_nothing() {
    let handler = setup().await;
    // Measured (cell n05): the source refuses `command cannot be given for
    // empty pane` before it places anything, because `-I` implies an empty
    // pane exactly as `-E` does.
    let before = window(&handler).await.pane_count();
    let result = send(
        &handler,
        offer(
            &["new-pane", "-I", "-d", "printf", "hello"],
            Some(b"x".to_vec()),
        ),
    )
    .await;
    let error = match result {
        NewPaneInputV1Response::NotAdmitted { error } => error.to_string(),
        NewPaneInputV1Response::Complete {
            error: Some(error), ..
        } => error.to_string(),
        other => panic!("{other:?}"),
    };
    assert!(
        error.contains("command cannot be given for empty pane"),
        "{error}"
    );
    assert_eq!(window(&handler).await.pane_count(), before);
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_input_flag_without_its_channel_is_refused_before_anything_is_created() {
    let handler = setup().await;
    // An ordinary queue, a sourced file or a key binding has no standard input
    // to offer. `-I` there has nothing to read, so it is refused before the
    // target is touched: the float is never created empty and then left so.
    let before = window(&handler).await.pane_count();
    let error = queue(&handler, "new-pane -I -d")
        .await
        .expect_err("no channel reaches an ordinary queue");
    assert!(error.to_string().contains("-I requires"), "{error}");
    assert_eq!(window(&handler).await.pane_count(), before);
    // The same through a real sourced file, which is the other entry of that
    // queue: a file that actually contains `-I`, on every platform.
    let root = temp_root("fstdin1-input-without-channel");
    let sourced = root.join("input.rmux");
    write_config(&sourced, "new-pane -I -d\n");
    let error = queue(&handler, &format!("source-file {}", shell_quote(&sourced)))
        .await
        .expect_err("a sourced file carries no client standard input either");
    assert!(error.to_string().contains("-I requires"), "{error}");
    assert_eq!(window(&handler).await.pane_count(), before);
    std::fs::remove_dir_all(&root).expect("the sourced fixture is removed");
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_offer_is_refused_without_its_capability_or_with_more_than_one_command() {
    let handler = setup().await;
    let before = window(&handler).await.pane_count();
    let mut without_capability = offer(&["new-pane", "-I", "-d"], Some(Vec::new()));
    without_capability.handshake = HandshakeRequest::current();
    let mut wrong_capability = without_capability.clone();
    wrong_capability.handshake = HandshakeRequest::requiring(["commands.new_pane.input.v999"]);
    for request in [
        without_capability,
        wrong_capability,
        offer(&["new-pane", "-I", "-d;", "kill-server"], Some(Vec::new())),
        offer(
            &["FSTDIN1_BAD=yes", "new-pane", "-I", "-d"],
            Some(Vec::new()),
        ),
        offer(&["split-window", "-d"], Some(Vec::new())),
    ] {
        assert!(
            matches!(
                send(&handler, request).await,
                NewPaneInputV1Response::NotAdmitted { .. }
            ),
            "the offer admits only one new-pane with its own capability"
        );
        assert_eq!(window(&handler).await.pane_count(), before);
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn every_capability_the_offer_uses_must_be_required_by_the_peer() {
    let handler = setup().await;
    let before = window(&handler).await.pane_count();
    // None of the three implies another, so dropping any one of them is a
    // refusal before anything is parsed, admitted or created.
    let promised = [
        rmux_proto::CAPABILITY_NEW_PANE_INPUT_V1,
        rmux_proto::CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
        rmux_proto::CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1,
    ];
    for missing in promised {
        let mut request = offer(&["new-pane", "-I", "-d"], Some(Vec::new()));
        request.handshake = HandshakeRequest::requiring(
            promised
                .into_iter()
                .filter(|capability| *capability != missing),
        );
        let result = send(&handler, request).await;
        let NewPaneInputV1Response::NotAdmitted { error } = result else {
            panic!("{missing} was not required: {result:?}");
        };
        assert!(error.to_string().contains(missing), "{missing}: {error}");
        assert_eq!(window(&handler).await.pane_count(), before, "{missing}");
    }
    // The complete offer is admitted and opens its float.
    let pane = displayed(&handler, b"three capabilities\n").await;
    assert_eq!(
        content(&handler, &pane).await,
        "three capabilities\n\n\n\n\n\n"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_replacement_between_parse_and_commit_never_receives_the_input() {
    let handler = setup().await;
    let pid = std::process::id();
    queue(&handler, "split-window -d -t fnp:0.0")
        .await
        .expect("a neighbour, so killing the current pane leaves the window");
    let pause = install_queue_exact_target_capture_pause(&handler, "new-pane");
    let cloned = handler.clone();
    let pending = tokio::spawn(async move {
        cloned
            .handle(Request::NewPaneInputV1(Box::new(offer(
                &["new-pane", "-I", "-d"],
                Some(b"never displayed\n".to_vec()),
            ))))
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), pause.reached.notified())
        .await
        .unwrap();
    queue(&handler, "respawn-pane -k -t fnp:0.0").await.unwrap();
    let before = window(&handler).await;
    pause.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(3), pending)
        .await
        .unwrap()
        .unwrap();
    let Response::NewPaneInputV1(result) = response else {
        panic!("offer result")
    };
    assert!(
        matches!(
            *result,
            NewPaneInputV1Response::Complete { error: Some(_), .. }
                | NewPaneInputV1Response::NotAdmitted { .. }
        ),
        "{result:?}"
    );
    let after = window(&handler).await;
    assert_eq!(after.pane_count(), before.pane_count());
    assert!(after.floating_stack().is_empty());
    let _ = pid;
    cleanup(&handler).await;
}
