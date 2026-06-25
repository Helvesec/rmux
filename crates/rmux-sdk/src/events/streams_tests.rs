use super::*;

#[test]
fn split_lines_buffers_partial_input_and_drops_trailing_newlines() {
    let mut buffer = Vec::new();
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    split_lines(&mut buffer, b"alpha\nbet", &mut out);
    assert_eq!(buffer, b"bet");
    assert_eq!(out.len(), 1);
    assert!(matches!(
        &out[0],
        PaneLineItem::Line { text } if text == "alpha"
    ));

    split_lines(&mut buffer, b"a\n", &mut out);
    assert!(buffer.is_empty());
    assert_eq!(out.len(), 2);
    assert!(matches!(
        &out[1],
        PaneLineItem::Line { text } if text == "beta"
    ));
}

#[test]
fn split_lines_emits_empty_line_on_consecutive_newlines() {
    let mut buffer = Vec::new();
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    split_lines(&mut buffer, b"\n\n", &mut out);
    assert!(buffer.is_empty());
    assert_eq!(out.len(), 2);
    for item in out {
        assert!(matches!(item, PaneLineItem::Line { text } if text.is_empty()));
    }
}

#[test]
fn split_lines_replaces_invalid_utf8_with_replacement_character() {
    let mut buffer = Vec::new();
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    split_lines(&mut buffer, b"\xffhello\n", &mut out);
    assert_eq!(out.len(), 1);
    let PaneLineItem::Line { text } = out.into_iter().next().unwrap() else {
        panic!("expected line item");
    };
    assert!(
        text.contains('\u{FFFD}'),
        "lossy UTF-8 must replace invalid bytes with U+FFFD; got `{text}`"
    );
    assert!(text.ends_with("hello"));
}

#[test]
fn split_lines_keeps_carriage_return_inside_line() {
    let mut buffer = Vec::new();
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    split_lines(&mut buffer, b"alpha\r\n", &mut out);
    assert_eq!(out.len(), 1);
    assert!(matches!(
        out.front().unwrap(),
        PaneLineItem::Line { text } if text == "alpha\r"
    ));
}

#[test]
fn pane_output_start_maps_to_proto_variants() {
    assert_eq!(
        PaneOutputStart::Now.into_proto(),
        PaneOutputSubscriptionStart::Now
    );
    assert_eq!(
        PaneOutputStart::Oldest.into_proto(),
        PaneOutputSubscriptionStart::Oldest
    );
}

#[test]
fn is_subscription_gone_matches_known_server_strings() {
    let gone = RmuxError::protocol(rmux_proto::RmuxError::Server(
        "subscription not found".to_owned(),
    ));
    let receiver_gone = RmuxError::protocol(rmux_proto::RmuxError::Server(
        "subscription receiver not found".to_owned(),
    ));
    let other = RmuxError::protocol(rmux_proto::RmuxError::Server(
        "different daemon error".to_owned(),
    ));
    assert!(is_subscription_gone(&gone));
    assert!(is_subscription_gone(&receiver_gone));
    assert!(!is_subscription_gone(&other));
}

#[test]
fn is_subscription_gone_does_not_match_ownership_or_invalid_target_errors() {
    // The daemon emits a separate "not owned by this connection"
    // error when a cursor is driven from the wrong transport. That
    // is a real protocol violation, not a subscription-gone signal,
    // so it must propagate as an SDK error rather than silently
    // ending the stream.
    let owned_elsewhere = RmuxError::protocol(rmux_proto::RmuxError::Server(
        "subscription is not owned by this connection".to_owned(),
    ));
    let invalid_target = RmuxError::protocol(rmux_proto::RmuxError::InvalidTarget {
        value: "alpha:0.0".to_owned(),
        reason: "pane index does not exist in session".to_owned(),
    });
    let session_not_found =
        RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound("alpha".to_owned()));
    assert!(!is_subscription_gone(&owned_elsewhere));
    assert!(!is_subscription_gone(&invalid_target));
    assert!(!is_subscription_gone(&session_not_found));
}

#[test]
fn split_lines_preserves_nul_byte_inside_rendered_text() {
    // NUL is valid UTF-8 (`U+0000`) and must round-trip through the
    // lossy decode that the line stream applies — only invalid byte
    // sequences are allowed to collapse to U+FFFD.
    let mut buffer = Vec::new();
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    split_lines(&mut buffer, b"a\0b\n", &mut out);
    assert_eq!(out.len(), 1);
    let PaneLineItem::Line { text } = out.into_iter().next().unwrap() else {
        panic!("expected line item");
    };
    assert_eq!(text, "a\0b");
    assert!(!text.contains('\u{FFFD}'));
}

#[test]
fn split_lines_reassembles_multibyte_codepoint_across_chunk_boundary() {
    // The daemon may chunk arbitrary byte boundaries. A two-byte
    // UTF-8 codepoint split across two cursor batches must NOT
    // produce a U+FFFD replacement when the LF arrives — the line
    // stream lossy-decodes the *complete* line, not each chunk.
    let mut buffer = Vec::new();
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    split_lines(&mut buffer, &[0xc3], &mut out); // first half of `é`
    assert!(out.is_empty(), "no LF yet, no line yielded");
    split_lines(&mut buffer, &[0xa9, b'\n'], &mut out);
    let PaneLineItem::Line { text } = out.into_iter().next().unwrap() else {
        panic!("expected line item");
    };
    assert_eq!(text, "é");
    assert!(!text.contains('\u{FFFD}'));
}

#[test]
fn split_lines_yields_many_lines_in_order_from_one_chunk() {
    // Multiple LFs in a single chunk must yield lines in protocol
    // order without reordering.
    let mut buffer = Vec::new();
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    split_lines(&mut buffer, b"one\ntwo\nthree\n", &mut out);
    let texts: Vec<String> = out
        .into_iter()
        .map(|item| match item {
            PaneLineItem::Line { text } => text,
            other => panic!("expected line item, got {other:?}"),
        })
        .collect();
    assert_eq!(texts, vec!["one", "two", "three"]);
    assert!(buffer.is_empty(), "trailing LF flushes the buffer");
}

#[test]
fn split_lines_forces_flush_at_line_buffer_limit() {
    let mut buffer = Vec::new();
    let mut force_flushed = false;
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    let chunk = vec![b'a'; LINE_BUFFER_MAX];

    split_lines_bounded(&mut buffer, &mut force_flushed, &chunk, &mut out);

    assert!(buffer.is_empty());
    assert!(force_flushed);
    assert_eq!(out.len(), 1);
    assert!(matches!(
        out.front().unwrap(),
        PaneLineItem::Line { text } if text.len() == LINE_BUFFER_MAX
    ));
}

#[test]
fn split_lines_suppresses_newline_after_forced_flush() {
    let mut buffer = Vec::new();
    let mut force_flushed = false;
    let mut out: VecDeque<PaneLineItem> = VecDeque::new();
    let chunk = vec![b'a'; LINE_BUFFER_MAX];

    split_lines_bounded(&mut buffer, &mut force_flushed, &chunk, &mut out);
    split_lines_bounded(&mut buffer, &mut force_flushed, b"\n\n", &mut out);

    let texts: Vec<String> = out
        .into_iter()
        .map(|item| match item {
            PaneLineItem::Line { text } => text,
            other => panic!("expected line item, got {other:?}"),
        })
        .collect();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0].len(), LINE_BUFFER_MAX);
    assert!(
        texts[1].is_empty(),
        "second LF still represents an empty line"
    );
}

#[test]
fn ingest_cursor_preserves_event_order_and_payload_bytes() {
    let mut pending: VecDeque<PaneOutputChunk> = VecDeque::new();
    ingest_cursor(
        &mut pending,
        vec![
            PaneOutputEvent {
                sequence: 5,
                bytes: vec![0xff, 0x00, b'a'],
            },
            PaneOutputEvent {
                sequence: 6,
                bytes: b"b\n".to_vec(),
            },
        ],
    );
    let chunk = pending.pop_front().expect("first event");
    match chunk {
        PaneOutputChunk::Bytes { sequence, bytes } => {
            assert_eq!(sequence, 5);
            assert_eq!(bytes, vec![0xff, 0x00, b'a']);
        }
        other => panic!("expected bytes chunk, got {other:?}"),
    }
    let chunk = pending.pop_front().expect("second event");
    match chunk {
        PaneOutputChunk::Bytes { sequence, bytes } => {
            assert_eq!(sequence, 6);
            assert_eq!(bytes, b"b\n");
        }
        other => panic!("expected bytes chunk, got {other:?}"),
    }
}

#[test]
fn pane_output_start_default_is_now() {
    // The default is `Now` — replaying the entire retained backlog
    // by accident on every stream open would be a surprising
    // performance footgun, so the default deliberately starts at
    // the live tail.
    assert_eq!(PaneOutputStart::default(), PaneOutputStart::Now);
}

#[test]
fn pane_recent_output_from_proto_round_trips_payload_and_sequences() {
    let proto = ProtoRecentOutput {
        bytes: vec![0xff, 0xfe, 0x00, b'!'],
        oldest_sequence: Some(11),
        newest_sequence: Some(13),
    };
    let recent = PaneRecentOutput::from_proto(proto);
    assert_eq!(recent.bytes, vec![0xff, 0xfe, 0x00, b'!']);
    assert_eq!(recent.oldest_sequence, Some(11));
    assert_eq!(recent.newest_sequence, Some(13));
}

#[test]
fn pane_lag_notice_from_proto_round_trips_all_fields() {
    let proto = ProtoLagNotice {
        expected_sequence: 3,
        resume_sequence: 9,
        missed_events: 6,
        newest_sequence: 12,
        recent: ProtoRecentOutput {
            bytes: b"abc".to_vec(),
            oldest_sequence: Some(8),
            newest_sequence: Some(9),
        },
    };
    let notice = PaneLagNotice::from_proto(proto);
    assert_eq!(notice.expected_sequence, 3);
    assert_eq!(notice.resume_sequence, 9);
    assert_eq!(notice.missed_events, 6);
    assert_eq!(notice.newest_sequence, 12);
    assert_eq!(notice.recent.bytes, b"abc");
    assert_eq!(notice.recent.oldest_sequence, Some(8));
    assert_eq!(notice.recent.newest_sequence, Some(9));
}
