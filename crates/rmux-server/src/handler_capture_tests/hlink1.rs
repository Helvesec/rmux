//! `capture-pane -H` publishes the links a pane holds, by identity.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux walks the cells of every captured line in their physical order,
//! keeps a pane's link identities in one list for the WHOLE capture, skips a
//! cell whose identity it has already written, separates the new links of one
//! line with a space, omits a line that carried no new link, and ends every
//! emitted line with a line feed (`cmd-capture-pane.c:110-145,247-302`). The
//! identity is the stored one: `Hyperlinks::put` deduplicates a non-empty
//! `id=` with its URI and gives every anonymous opening an identity of its own
//! (`hyperlinks.c:133-176` in the primary, `hyperlinks.rs:46-85` here).
//!
//! The fixture parses real OSC 8 bytes with the real parser into a real
//! `Screen`, and every witness reads them back through the real
//! `capture-pane` request and the buffer it writes. Nothing here injects a
//! rendered escape chain into a getter.

use super::*;

const URI_A: &str = "https://hlink-a.example/a";
const URI_B: &str = "https://hlink-b.example/b";

/// The three physical lines of the journey, as a tool would print them.
///
/// Line 0 opens two identities of two addresses; line 1 opens the first
/// identity again; line 2 opens a third identity of the first address and then
/// two anonymous openings of it, which the store keeps apart.
fn three_lines() -> Vec<u8> {
    format!(
        "\x1b]8;id=a;{URI_A}\x1b\\A1\x1b]8;;\x1b\\ \x1b]8;id=b;{URI_B}\x1b\\B1\x1b]8;;\x1b\\\r\n\
         \x1b]8;id=a;{URI_A}\x1b\\A2\x1b]8;;\x1b\\\r\n\
         \x1b]8;id=c;{URI_A}\x1b\\C1\x1b]8;;\x1b\\ \x1b]8;;{URI_A}\x1b\\N1\x1b]8;;\x1b\\ \x1b]8;;{URI_A}\x1b\\N2\x1b]8;;\x1b\\\r\n"
    )
    .into_bytes()
}

/// Captures the links of the first three lines into a named buffer and returns
/// what that buffer holds, byte for byte.
async fn captured_links(handler: &RequestHandler, target: &PaneTarget, buffer: &str) -> Vec<u8> {
    let mut request = capture_pane_request(target.clone(), Some(0), Some(2), false, Some(buffer));
    request.hyperlinks = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(request)))
        .await;
    assert!(
        matches!(response, Response::CapturePane(_)),
        "capture-pane -H answered {response:?}"
    );
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some(buffer.to_owned()),
        }))
        .await;
    let Response::ShowBuffer(show) = show else {
        panic!("show-buffer answered {show:?}")
    };
    show.command_output().stdout().to_vec()
}

/// A pane of that session holding the three lines above.
async fn pane_with_links(handler: &RequestHandler, name: &str) -> PaneTarget {
    create_session(handler, name).await;
    let target = PaneTarget::with_window(session_name(name), 0, 0);
    replace_transcript_contents(
        handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        &three_lines(),
    )
    .await;
    target
}

#[tokio::test]
async fn two_links_on_one_line_are_one_line_of_the_capture() {
    let handler = RequestHandler::new();
    let target = pane_with_links(&handler, "hlink-grouped").await;

    // The first line opened two identities: they are the new links of that one
    // line, separated by a space, and the line ends there.
    let captured = captured_links(&handler, &target, "HLINK-GROUPED").await;
    let first = captured
        .split(|byte| *byte == b'\n')
        .next()
        .expect("the capture has a first line")
        .to_vec();
    assert_eq!(
        String::from_utf8(first).expect("the capture is utf-8"),
        format!("{URI_A} {URI_B}")
    );
}

#[tokio::test]
async fn the_same_identity_seen_again_is_not_captured_twice() {
    let handler = RequestHandler::new();
    let target = pane_with_links(&handler, "hlink-repeated").await;

    // The second line opens the identity the first line already carried, so it
    // holds no new link and is not in the capture at all: what follows the
    // first line is the third line's own links.
    let captured = captured_links(&handler, &target, "HLINK-REPEATED").await;
    assert_eq!(
        String::from_utf8(captured).expect("the capture is utf-8"),
        format!("{URI_A} {URI_B}\n{URI_A} {URI_A} {URI_A}\n")
    );
}

#[tokio::test]
async fn distinct_identities_of_one_address_stay_distinct_links() {
    let handler = RequestHandler::new();
    let target = pane_with_links(&handler, "hlink-anonymous").await;

    // The third line opens one named identity and two anonymous ones, all of
    // the same address: the store keeps them apart, so the capture carries
    // three links and not one.
    let captured = captured_links(&handler, &target, "HLINK-ANONYMOUS").await;
    let last = captured
        .split(|byte| *byte == b'\n')
        .rfind(|line| !line.is_empty())
        .expect("the capture has a last line")
        .to_vec();
    assert_eq!(
        String::from_utf8(last).expect("the capture is utf-8"),
        format!("{URI_A} {URI_A} {URI_A}")
    );
}

#[tokio::test]
async fn a_pane_without_links_captures_nothing() {
    let handler = RequestHandler::new();
    create_session(&handler, "hlink-plain").await;
    let target = PaneTarget::with_window(session_name("hlink-plain"), 0, 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"plain one\r\nplain two\r\nplain three\r\n",
    )
    .await;

    // Every line of this pane carries no link, so every line is omitted and
    // nothing is left to write. An empty capture writes no buffer at all -
    // the store's own rule for empty content, which this lot does not touch -
    // so the buffer this request named is simply not there afterwards.
    let mut request =
        capture_pane_request(target.clone(), Some(0), Some(2), false, Some("HLINK-PLAIN"));
    request.hyperlinks = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(request)))
        .await;
    assert!(
        matches!(response, Response::CapturePane(_)),
        "capture-pane -H answered {response:?}"
    );
    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("HLINK-PLAIN".to_owned()),
        }))
        .await;
    assert!(
        matches!(show, Response::Error(_)),
        "an empty capture wrote a buffer: {show:?}"
    );
}

/// Captures the links of a pane with `-H` and one prefixing flag, into a buffer
/// of its own, and returns what `show-buffer` answers for that name afterwards.
async fn captured_links_with_prefix(
    handler: &RequestHandler,
    target: &PaneTarget,
    buffer: &str,
    line_numbers: bool,
    include_format: bool,
) -> Response {
    let mut request = capture_pane_request(target.clone(), Some(0), Some(2), false, Some(buffer));
    request.hyperlinks = true;
    request.line_numbers = line_numbers;
    request.include_format = include_format;
    let response = handler
        .handle(Request::CapturePane(Box::new(request)))
        .await;
    assert!(
        matches!(response, Response::CapturePane(_)),
        "capture-pane -H answered {response:?}"
    );
    handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some(buffer.to_owned()),
        }))
        .await
}

#[tokio::test]
async fn an_empty_link_capture_stays_empty_under_the_prefixing_flags() {
    let handler = RequestHandler::new();
    create_session(&handler, "hlink-prefix").await;
    let target = PaneTarget::with_window(session_name("hlink-prefix"), 0, 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"plain one\r\nplain two\r\nplain three\r\n",
    )
    .await;

    // The primary skips a line that carried no new link BEFORE it writes any
    // number or flag (`cmd-capture-pane.c:255-258`), so a pane without links
    // leaves nothing at all to prefix and the capture stays empty - whichever
    // of the two prefixing flags was asked for. An empty capture writes no
    // buffer, so each name is afterwards unknown to the store, and the store
    // says exactly which name it does not have.
    for (buffer, line_numbers, include_format) in [
        ("HLINK-PREFIX-NUMBERS", true, false),
        ("HLINK-PREFIX-FLAGS", false, true),
    ] {
        let show =
            captured_links_with_prefix(&handler, &target, buffer, line_numbers, include_format)
                .await;
        let Response::Error(error) = show else {
            panic!("-H with line_numbers={line_numbers} include_format={include_format} wrote a buffer: {show:?}");
        };
        assert_eq!(
            error.error.to_string(),
            format!("server error: no buffer {buffer}"),
            "the store refused {buffer} for another reason"
        );
    }
}
