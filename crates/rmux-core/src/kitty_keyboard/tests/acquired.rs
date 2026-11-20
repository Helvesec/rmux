use super::{
    assert_event, corpus_row, corpus_rows, decode_hex, event_from_expectation,
    selected_encoder_row, string_field, INVALID_EVENT_ROWS,
};
use crate::kitty_keyboard::{
    AcquiredDecoder, AcquiredItem, Error, Event, Function, Key, Rejection, FRAME_LIMIT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnedItem {
    Event(Event),
    Other(Vec<u8>),
    Rejected(Rejection),
}

#[test]
fn acquired_fragmentation_rows_are_transport_boundary_invariant() {
    let rows: Vec<_> = corpus_rows()
        .filter(|row| {
            let id = string_field(row, "id");
            id.starts_with("R-FRAG-") && id != "R-FRAG-INVALID"
        })
        .collect();
    assert_eq!(rows.len(), 28);

    for row in rows {
        let id = string_field(&row, "id");
        let chunks: Vec<_> = row["chunks_hex"]
            .as_array()
            .unwrap_or_else(|| panic!("{id}: missing chunks"))
            .iter()
            .map(|chunk| decode_hex(chunk.as_str().unwrap()))
            .collect();
        let items = decode_chunks(chunks.iter().map(Vec::as_slice));
        let actual = event_items(&items, id);
        let expected: Vec<_> = row["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(event_from_expectation)
            .collect();
        assert_eq!(actual, expected, "{id}");
    }
}

#[test]
fn acquired_reencode_rows_emit_independently_asserted_semantics_for_all_cuts() {
    let rows: Vec<_> = corpus_rows()
        .filter(|row| {
            selected_encoder_row(row) && string_field(row, "direction") == "event-reencode"
        })
        .collect();
    assert_eq!(rows.len(), 81);

    for row in rows {
        let id = string_field(&row, "id");
        let frame = decode_hex(string_field(&row, "input_hex"));
        for_each_segmentation(&frame, |label, items| {
            let [OwnedItem::Event(event)] = items else {
                panic!("{id}/{label}: unexpected items {items:?}");
            };
            assert_event(event, &row["event"], &frame, &format!("{id}/{label}"));
        });
    }
}

#[test]
fn acquired_malformed_frames_quarantine_tails_and_recover_for_all_cuts() {
    assert_eq!(INVALID_EVENT_ROWS.len(), 21);
    let mut recovery_rows = 0;
    for (id, expected_error) in INVALID_EVENT_ROWS {
        let row = corpus_row(id);
        let invalid = decode_hex(string_field(&row, "input_hex"));
        let Some(suffix_hex) = row["recovery_suffix_hex"].as_str() else {
            for_each_segmentation(&invalid, |label, items| {
                assert_eq!(
                    items,
                    &[OwnedItem::Rejected(Rejection {
                        error: *expected_error,
                        bytes: invalid.len(),
                    })],
                    "{id}/{label}"
                );
            });
            continue;
        };
        recovery_rows += 1;
        let suffix = decode_hex(suffix_hex);
        let mut stream = invalid.clone();
        stream.extend_from_slice(&suffix);
        let recovered = event_from_expectation(&row["recovered_event"]);

        for_each_segmentation(&stream, |label, items| {
            assert_eq!(
                items,
                &[
                    OwnedItem::Rejected(Rejection {
                        error: *expected_error,
                        bytes: invalid.len(),
                    }),
                    OwnedItem::Event(recovered.clone()),
                ],
                "{id}/{label}"
            );
        });
    }
    assert_eq!(recovery_rows, 20);
}

#[test]
fn acquired_recovery_distinguishes_interruption_cpr_f3_and_unassigned_pua() {
    let nested = corpus_row("R-RECOVERY-NESTED-CSI");
    let nested_bytes = decode_hex(string_field(&nested, "input_hex"));
    for_each_segmentation(&nested_bytes, |label, items| {
        assert_eq!(
            items,
            &[
                OwnedItem::Rejected(Rejection {
                    error: Error::Interrupted,
                    bytes: 5,
                }),
                OwnedItem::Event(event_from_expectation(
                    &corpus_row("R-INVALID-NO-KEY")["recovered_event"],
                )),
            ],
            "nested/{label}"
        );
    });

    let cpr = decode_hex(string_field(&corpus_row("R-RECOVERY-CPR-F3"), "input_hex"));
    let mut cpr_then_f3 = cpr.clone();
    cpr_then_f3.extend_from_slice(b"\x1b[13~");
    for_each_segmentation(&cpr_then_f3, |label, items| {
        let [OwnedItem::Other(other), OwnedItem::Event(event)] = items else {
            panic!("cpr-f3/{label}: unexpected items {items:?}");
        };
        assert_eq!(other, &cpr, "cpr-f3/{label}");
        assert_eq!(
            event.key,
            Key::Function(Function::named("F3").unwrap()),
            "cpr-f3/{label}"
        );
    });

    let unassigned = decode_hex(string_field(
        &corpus_row("R-RECOVERY-UNASSIGNED"),
        "input_hex",
    ));
    for_each_segmentation(&unassigned, |label, items| {
        let [OwnedItem::Event(event)] = items else {
            panic!("unassigned/{label}: unexpected items {items:?}");
        };
        assert_eq!(event.key, Key::UnassignedPua('\u{e000}'), "{label}");
    });
}

#[test]
fn acquired_neighbors_and_bracketed_paste_remain_other_bytes_for_all_cuts() {
    const OTHER_ROWS: &[&str] = &[
        "R-NEIGHBOR-FOCUS-IN",
        "R-NEIGHBOR-FOCUS-OUT",
        "R-NEIGHBOR-SGR-MOUSE",
        "R-NEIGHBOR-CPR",
        "R-NEIGHBOR-CPR-ROW1-COL5",
        "R-MOK-KITTY-POP",
        "R-NEIGHBOR-BRACKETED-PASTE",
    ];

    for id in OTHER_ROWS {
        let bytes = decode_hex(string_field(&corpus_row(id), "input_hex"));
        for_each_segmentation(&bytes, |label, items| {
            assert_only_other(items, &bytes, &format!("{id}/{label}"));
        });
    }

    let modify_other_keys = b"\x1b[27;5;97~";
    for_each_segmentation(modify_other_keys, |label, items| {
        assert_only_other(items, modify_other_keys, &format!("mok/{label}"));
    });
}

#[test]
fn acquired_expiry_is_caller_triggered_and_storage_remains_bounded() {
    let timeout = corpus_row("R-INVALID-FRAGMENT-TIMEOUT");
    assert!(timeout["trigger"]["elapsed_ms"].is_null());
    let partial = decode_hex(string_field(&timeout, "input_hex"));
    let mut decoder = AcquiredDecoder::new();
    let mut items = Vec::new();
    feed_owned(&mut decoder, &partial, &mut items);
    assert!(items.is_empty());
    assert_eq!(decoder.retained(), partial);
    assert_eq!(decoder.pending_len(), partial.len());
    assert_eq!(
        decoder.expire(),
        Some(Rejection {
            error: Error::Incomplete,
            bytes: partial.len(),
        })
    );
    assert!(decoder.retained().is_empty());
    assert_eq!(decoder.pending_len(), 0);
    assert_eq!(decoder.expire(), None);

    let oversized_partial = [b"\x1b[".as_slice(), &vec![b'9'; FRAME_LIMIT + 19]].concat();
    feed_owned(&mut decoder, &oversized_partial, &mut items);
    assert_eq!(decoder.retained().len(), FRAME_LIMIT);
    assert_eq!(decoder.pending_len(), oversized_partial.len());
    assert_eq!(
        decoder.expire(),
        Some(Rejection {
            error: Error::FrameLimit,
            bytes: oversized_partial.len(),
        })
    );

    feed_owned(&mut decoder, b"\x1b[98u", &mut items);
    assert!(matches!(items.last(), Some(OwnedItem::Event(_))));
}

#[test]
fn acquired_plain_and_non_csi_escape_bytes_keep_order_without_owned_results() {
    let bytes = b"plain\x1bx tail\x1b[12;34R done";
    for_each_segmentation(bytes, |label, items| {
        assert_only_other(items, bytes, label);
    });
}

#[test]
fn acquired_control_strings_shield_embedded_events_for_all_cuts() {
    const STRINGS: &[&[u8]] = &[
        b"\x1b]0;title\x1b[97u\x07",
        b"\x1b]0;title\x1b[97u\x1b\\",
        b"\x1bP1$rpayload\x1b[97u\x1b\\",
        b"\x1bXpayload\x1b[97u\x1b\\",
        b"\x1b^payload\x1b[97u\x1b\\",
        b"\x1b_payload\x1b[97u\x1b\\",
    ];

    for control in STRINGS {
        let mut bytes = control.to_vec();
        bytes.extend_from_slice(b"\x1b[98u");
        for_each_segmentation(&bytes, |label, items| {
            let event_index = items.len() - 1;
            let OwnedItem::Event(event) = &items[event_index] else {
                panic!("{label}: missing event after opaque string in {items:?}");
            };
            assert_eq!(event.key, Key::Scalar('b'), "{label}");
            assert_only_other(&items[..event_index], control, label);
        });
    }
}

#[test]
fn acquired_foreign_csi_responses_preserve_following_input_for_all_cuts() {
    const RESPONSES: &[&[u8]] = &[
        b"\x1b[0n",
        b"\x1b[1;2c",
        b"\x1b[4;24;80t",
        b"\x1b[2004;1$y",
        b"\x1b[0 q",
        b"\x1b[2;1;1;112;112;1;0x",
    ];

    for response in RESPONSES {
        assert_foreign_response_preserves_suffix(response);
    }
}

#[test]
fn acquired_complete_foreign_csi_grammar_is_not_a_final_allowlist() {
    const INTERMEDIATES: &[u8] = b" !\"#$%&'()*+,-./";
    let bodies = [
        b"1;2:3".to_vec(),
        [b"1;2".as_slice(), INTERMEDIATES].concat(),
    ];

    for final_byte in 0x40..=0x7e {
        if b"u~ABCDEFHPQS".contains(&final_byte) {
            continue;
        }
        for body in &bodies {
            let mut response = b"\x1b[".to_vec();
            response.extend_from_slice(body);
            response.push(final_byte);
            assert_foreign_response_preserves_suffix(&response);
        }
    }
}

#[test]
fn acquired_preserved_foreign_finals_outrank_trailing_separator_ambiguity() {
    const PRESERVED_FINALS: &[u8] = b"mMIORZhlpcnty";
    const TRAILING_EMPTY_BODIES: &[&[u8]] = &[b"0;", b"0:", b";", b"1;;"];

    for final_byte in PRESERVED_FINALS {
        for body in TRAILING_EMPTY_BODIES {
            let mut response = b"\x1b[".to_vec();
            response.extend_from_slice(body);
            response.push(*final_byte);
            assert_foreign_response_preserves_suffix(&response);
        }
    }
}

#[test]
fn acquired_exact_limit_preserved_foreign_final_does_not_consume_its_neighbor() {
    let mut response = b"\x1b[".to_vec();
    response.extend(std::iter::repeat_n(b'1', FRAME_LIMIT - 4));
    response.extend_from_slice(b";n");
    assert_eq!(response.len(), FRAME_LIMIT);

    let mut stream = response.clone();
    stream.extend_from_slice(b"hello\x1b[98u");
    for chunks in [
        vec![stream.as_slice()],
        stream.as_slice().chunks(1).collect(),
    ] {
        let items = decode_chunks(chunks);
        let event_index = items.len() - 1;
        assert!(
            matches!(&items[event_index], OwnedItem::Event(event) if event.key == Key::Scalar('b'))
        );
        let mut expected = response.clone();
        expected.extend_from_slice(b"hello");
        assert_only_other(&items[..event_index], &expected, "exact-limit");
    }
}

#[test]
fn acquired_foreign_boundary_keeps_malformed_keyboard_quarantine() {
    let malformed = b"\x1b[97;x1u\x1b[98u";
    for_each_segmentation(malformed, |label, items| {
        assert_eq!(
            items,
            &[
                OwnedItem::Rejected(Rejection {
                    error: Error::Decimal,
                    bytes: 8,
                }),
                OwnedItem::Event(event_from_expectation(
                    &corpus_row("R-INVALID-BAD-ASCII")["recovered_event"],
                )),
            ],
            "bad-ascii/{label}"
        );
    });

    let malformed_order = b"\x1b[1$2yhello\x1b[98u";
    for_each_segmentation(malformed_order, |label, items| {
        let Some((OwnedItem::Rejected(rejection), tail)) = items.split_first() else {
            panic!("parameter-after-intermediate/{label}: unexpected items {items:?}");
        };
        let Some((OwnedItem::Event(event), other)) = tail.split_last() else {
            panic!("parameter-after-intermediate/{label}: unexpected items {items:?}");
        };
        assert_eq!(
            rejection,
            &Rejection {
                error: Error::InvalidFrame,
                bytes: 6,
            },
            "parameter-after-intermediate/{label}"
        );
        assert_only_other(
            other,
            b"hello",
            &format!("parameter-after-intermediate/{label}"),
        );
        assert_eq!(
            event.key,
            Key::Scalar('b'),
            "parameter-after-intermediate/{label}"
        );
    });
}

#[test]
fn acquired_oversized_foreign_csi_envelopes_follow_bounded_rejection_policy() {
    for final_byte in 0x40..=0x7e {
        if b"u~ABCDEFHPQS".contains(&final_byte) {
            continue;
        }
        let mut response = b"\x1b[".to_vec();
        response.extend(std::iter::repeat_n(b'1', FRAME_LIMIT));
        response.push(final_byte);
        let response_len = response.len();
        response.extend_from_slice(b"hello\x1b[98u");

        let mut decoder = AcquiredDecoder::new();
        let mut items = Vec::new();
        for chunk in response.chunks(13) {
            feed_owned(&mut decoder, chunk, &mut items);
            assert!(decoder.retained().len() <= FRAME_LIMIT);
        }
        assert_eq!(
            items,
            [
                OwnedItem::Rejected(Rejection {
                    error: Error::FrameLimit,
                    bytes: response_len,
                }),
                OwnedItem::Other(b"hello".to_vec()),
                OwnedItem::Event(event_from_expectation(
                    &corpus_row("R-INVALID-BAD-ASCII")["recovered_event"],
                )),
            ],
            "final byte {final_byte:#x}"
        );
    }
}

#[test]
fn acquired_control_string_partial_st_is_opaque_and_not_expirable() {
    let mut decoder = AcquiredDecoder::new();
    let mut items = Vec::new();
    feed_owned(&mut decoder, b"\x1bP1$rpayload\x1b", &mut items);
    assert_eq!(decoder.expire(), None);
    assert!(decoder.retained().is_empty());
    assert_eq!(decoder.pending_len(), 0);

    feed_owned(&mut decoder, b"\\\x1b[98u", &mut items);
    let event_index = items.len() - 1;
    assert_only_other(
        &items[..event_index],
        b"\x1bP1$rpayload\x1b\\",
        "partial-st",
    );
    let OwnedItem::Event(event) = &items[event_index] else {
        panic!("missing event after partial ST: {items:?}");
    };
    assert_eq!(event.key, Key::Scalar('b'));
}

#[test]
fn acquired_oversized_control_string_is_streamed_without_retention() {
    let mut decoder = AcquiredDecoder::new();
    let mut items = Vec::new();
    feed_owned(&mut decoder, b"\x1bP", &mut items);
    for chunk in vec![b'x'; FRAME_LIMIT + 19].chunks(17) {
        feed_owned(&mut decoder, chunk, &mut items);
        assert!(decoder.retained().is_empty());
        assert_eq!(decoder.pending_len(), 0);
    }
    assert_eq!(decoder.expire(), None);
    feed_owned(&mut decoder, b"\x1b\\\x1b[98u", &mut items);
    assert!(matches!(items.last(), Some(OwnedItem::Event(event)) if event.key == Key::Scalar('b')));
}

#[test]
fn acquired_oversized_foreign_namespace_is_streamed_without_retention_or_loss() {
    let mut bytes = b"\x1b[?".to_vec();
    bytes.extend(std::iter::repeat_n(b'9', FRAME_LIMIT + 19));
    bytes.extend_from_slice(b"u\x1b[98u");

    let items = decode_chunks(bytes.chunks(1));
    let event_index = items.len() - 1;
    assert!(matches!(items[event_index], OwnedItem::Event(_)));
    assert_only_other(&items[..event_index], &bytes[..bytes.len() - 5], "foreign");
}

#[test]
fn acquired_interrupted_foreign_namespace_preserves_bytes_and_finds_next_event() {
    let bytes = b"\x1b[?12\x1b[98u";
    for_each_segmentation(bytes, |label, items| {
        let event_index = items.len() - 1;
        let OwnedItem::Event(event) = &items[event_index] else {
            panic!("{label}: missing recovered event in {items:?}");
        };
        assert_eq!(event.key, Key::Scalar('b'), "{label}");
        assert_only_other(&items[..event_index], &bytes[..5], label);
    });
}

fn for_each_segmentation(input: &[u8], mut assert_items: impl FnMut(&str, &[OwnedItem])) {
    let whole = decode_chunks(std::iter::once(input));
    assert_items("whole", &whole);

    for cut in 1..input.len() {
        let items = decode_chunks([&input[..cut], &input[cut..]]);
        assert_items(&format!("cut-{cut}"), &items);
    }

    let bytewise = decode_chunks(input.chunks(1));
    assert_items("bytewise", &bytewise);
}

fn decode_chunks<'a>(chunks: impl IntoIterator<Item = &'a [u8]>) -> Vec<OwnedItem> {
    let mut decoder = AcquiredDecoder::new();
    let mut items = Vec::new();
    for chunk in chunks {
        feed_owned(&mut decoder, chunk, &mut items);
    }
    assert_eq!(decoder.pending_len(), 0, "unexpected incomplete candidate");
    items
}

fn feed_owned(decoder: &mut AcquiredDecoder, bytes: &[u8], items: &mut Vec<OwnedItem>) {
    decoder.feed(bytes, |item| {
        items.push(match item {
            AcquiredItem::Event(event) => OwnedItem::Event(event),
            AcquiredItem::Other(bytes) => OwnedItem::Other(bytes.to_vec()),
            AcquiredItem::Rejected(rejection) => OwnedItem::Rejected(rejection),
        });
    });
}

fn event_items(items: &[OwnedItem], id: &str) -> Vec<Event> {
    items
        .iter()
        .map(|item| match item {
            OwnedItem::Event(event) => event.clone(),
            other => panic!("{id}: unexpected non-event {other:?}"),
        })
        .collect()
}

fn assert_only_other(items: &[OwnedItem], expected: &[u8], label: &str) {
    let mut actual = Vec::new();
    for item in items {
        match item {
            OwnedItem::Other(bytes) => actual.extend_from_slice(bytes),
            other => panic!("{label}: unexpected non-other {other:?}"),
        }
    }
    assert_eq!(actual, expected, "{label}");
}

fn assert_foreign_response_preserves_suffix(response: &[u8]) {
    let mut bytes = response.to_vec();
    bytes.extend_from_slice(b"hello\x1b[98u");
    for_each_segmentation(&bytes, |label, items| {
        let event_index = items.len() - 1;
        let OwnedItem::Event(event) = &items[event_index] else {
            panic!("{label}: missing event after foreign CSI in {items:?}");
        };
        assert_eq!(event.key, Key::Scalar('b'), "{label}");

        let mut expected = response.to_vec();
        expected.extend_from_slice(b"hello");
        assert_only_other(&items[..event_index], &expected, label);
    });
}
