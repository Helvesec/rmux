//! Strict, bounded OSC 52 clipboard query/response codec.

use rmux_core::{input::InputEndType, TerminalClipboardQuery};
use rmux_proto::DEFAULT_MAX_FRAME_LENGTH;

use crate::outer_terminal::encode_base64;

pub(crate) const CLIPBOARD_QUERY_SEQUENCE: &[u8] = b"\x1b]52;;?\x07";
const MAX_ATTACHED_CLIPBOARD_SEQUENCE_BYTES: usize = DEFAULT_MAX_FRAME_LENGTH;
// Pane OSC strings are already bounded by rmux-core's historical 8 MiB
// terminal-passthrough ceiling. Keep that established write capacity while
// attached terminal responses remain constrained by their 1 MiB data frame.
const MAX_PANE_CLIPBOARD_SEQUENCE_BYTES: usize = 8 * 1024 * 1024;
const VALID_SELECTIONS: &[u8] = b"cpqs01234567";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedClipboardResponse {
    pub(crate) selection: Option<u8>,
    pub(crate) content: Vec<u8>,
}

pub(crate) fn encode_clipboard_response(
    query: TerminalClipboardQuery,
    content: &[u8],
) -> Option<Vec<u8>> {
    encode_clipboard_response_parts(query.selection(), query.terminator(), content)
}

pub(crate) fn encode_clipboard_response_parts(
    selection: Option<u8>,
    terminator: InputEndType,
    content: &[u8],
) -> Option<Vec<u8>> {
    if content.is_empty() {
        return None;
    }
    if selection.is_some_and(|selection| !VALID_SELECTIONS.contains(&selection)) {
        return None;
    }
    let encoded_len = content
        .len()
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?;
    let terminator_len = match terminator {
        InputEndType::Bel => 1,
        InputEndType::St => 2,
    };
    let sequence_len = 6usize
        .checked_add(usize::from(selection.is_some()))?
        .checked_add(encoded_len)?
        .checked_add(terminator_len)?;
    if sequence_len > MAX_PANE_CLIPBOARD_SEQUENCE_BYTES {
        return None;
    }

    let mut response = Vec::with_capacity(sequence_len);
    response.extend_from_slice(b"\x1b]52;");
    if let Some(selection) = selection {
        response.push(selection);
    }
    response.push(b';');
    response.extend_from_slice(encode_base64(content).as_bytes());
    match terminator {
        InputEndType::Bel => response.push(b'\x07'),
        InputEndType::St => response.extend_from_slice(b"\x1b\\"),
    }
    Some(response)
}

pub(crate) fn decode_clipboard_response_body(body: &[u8]) -> Option<DecodedClipboardResponse> {
    // The caller strips the OSC prefix and terminator. Bound for the larger
    // ST form so accepting a body can never produce an over-limit sequence.
    if body.len().saturating_add(b"\x1b]52;\x1b\\".len()) > MAX_ATTACHED_CLIPBOARD_SEQUENCE_BYTES {
        return None;
    }
    let separator = body.iter().position(|byte| *byte == b';')?;
    let (selection, encoded_with_separator) = body.split_at(separator);
    // tmux accepts the payload even when the terminal returns an invalid or
    // multi-byte selection, but reflects a selection only when it is exactly
    // one byte. Keep the accepted byte allowlisted before reflecting it.
    let selection = match selection {
        [selection] if VALID_SELECTIONS.contains(selection) => Some(*selection),
        _ => None,
    };
    let encoded = encoded_with_separator.get(1..)?;
    let decoded =
        decode_base64_standard_with_limit(encoded, MAX_ATTACHED_CLIPBOARD_SEQUENCE_BYTES)?;
    (!decoded.is_empty()).then_some(DecodedClipboardResponse {
        selection,
        content: decoded,
    })
}

pub(crate) fn decode_pane_clipboard_write_payload(input: &[u8]) -> Option<Vec<u8>> {
    decode_base64_standard_with_limit(input, MAX_PANE_CLIPBOARD_SEQUENCE_BYTES)
}

fn decode_base64_standard_with_limit(input: &[u8], max_input_bytes: usize) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() > max_input_bytes {
        return None;
    }

    let mut symbol_end = input.len();
    let mut padding = 0usize;
    while symbol_end > 0 && input[symbol_end - 1] == b'=' {
        symbol_end -= 1;
        padding += 1;
    }
    if padding > 2 {
        return None;
    }
    let symbols = &input[..symbol_end];
    let remainder = symbols.len() % 4;
    if remainder == 1 {
        return None;
    }
    if padding > 0
        && (!input.len().is_multiple_of(4) || !matches!((remainder, padding), (2, 2) | (3, 1)))
    {
        return None;
    }

    let decoded_len = symbols.len().checked_mul(6)?.checked_div(8)?;
    if decoded_len > max_input_bytes {
        return None;
    }
    let mut output = Vec::with_capacity(decoded_len);
    let mut accumulator = 0u32;
    let mut retained_bits = 0u32;
    for &byte in symbols {
        accumulator = (accumulator << 6) | symbol_value(byte)?;
        retained_bits += 6;
        while retained_bits >= 8 {
            retained_bits -= 8;
            output.push(((accumulator >> retained_bits) & 0xff) as u8);
            accumulator &= retained_mask(retained_bits);
        }
    }
    (accumulator == 0).then_some(output)
}

fn symbol_value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

const fn retained_mask(bits: u32) -> u32 {
    if bits == 0 {
        0
    } else {
        (1u32 << bits) - 1
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::{input::InputEndType, TerminalClipboardQuery};

    use super::{
        decode_clipboard_response_body, decode_pane_clipboard_write_payload,
        encode_clipboard_response, DecodedClipboardResponse, MAX_ATTACHED_CLIPBOARD_SEQUENCE_BYTES,
        MAX_PANE_CLIPBOARD_SEQUENCE_BYTES,
    };

    #[test]
    fn response_encoding_preserves_typed_selector_and_terminator() {
        assert_eq!(
            encode_clipboard_response(
                TerminalClipboardQuery::new("zzpc", InputEndType::Bel),
                b"oracle-data",
            )
            .as_deref(),
            Some(b"\x1b]52;p;b3JhY2xlLWRhdGE=\x07".as_slice())
        );
        assert_eq!(
            encode_clipboard_response(
                TerminalClipboardQuery::new("c", InputEndType::St),
                b"outer-data",
            )
            .as_deref(),
            Some(b"\x1b]52;c;b3V0ZXItZGF0YQ==\x1b\\".as_slice())
        );
    }

    #[test]
    fn strict_response_decoder_accepts_canonical_or_unpadded_base64() {
        assert_eq!(
            decode_clipboard_response_body(b"c;b3V0ZXItZGF0YQ=="),
            Some(DecodedClipboardResponse {
                selection: Some(b'c'),
                content: b"outer-data".to_vec(),
            })
        );
        assert_eq!(
            decode_clipboard_response_body(b";b3V0ZXItZGF0YQ"),
            Some(DecodedClipboardResponse {
                selection: None,
                content: b"outer-data".to_vec(),
            })
        );
    }

    #[test]
    fn response_decoder_accepts_payload_but_drops_unsafe_or_multi_byte_selection() {
        for body in [
            b"x;b3V0ZXI=".as_slice(),
            b"cp;b3V0ZXI=".as_slice(),
            b"ppc;b3V0ZXI=".as_slice(),
        ] {
            assert_eq!(
                decode_clipboard_response_body(body),
                Some(DecodedClipboardResponse {
                    selection: None,
                    content: b"outer".to_vec(),
                }),
                "body={body:?}"
            );
        }
    }

    #[test]
    fn strict_response_decoder_rejects_empty_malformed_and_ambiguous_payloads() {
        for body in [
            b"c;".as_slice(),
            b"c;?".as_slice(),
            b"c;%%%".as_slice(),
            b"c;AB==".as_slice(),
            b"c;YQ=".as_slice(),
            b"c;YQ==extra".as_slice(),
        ] {
            assert_eq!(decode_clipboard_response_body(body), None, "body={body:?}");
        }
        assert_eq!(
            decode_pane_clipboard_write_payload(b"YQ==").as_deref(),
            Some(b"a".as_slice())
        );
        assert_eq!(
            decode_pane_clipboard_write_payload(b"YQ").as_deref(),
            Some(b"a".as_slice())
        );
    }

    #[test]
    fn pane_response_encoder_preserves_the_historical_eight_mibibyte_boundary() {
        let encoded_len = MAX_PANE_CLIPBOARD_SEQUENCE_BYTES - 8;
        assert_eq!(encoded_len % 4, 0);
        let mut maximum_content = vec![0; encoded_len / 4 * 3];
        let maximum_response = encode_clipboard_response(
            TerminalClipboardQuery::new("c", InputEndType::Bel),
            &maximum_content,
        )
        .expect("exactly bounded pane response encodes");
        assert_eq!(maximum_response.len(), MAX_PANE_CLIPBOARD_SEQUENCE_BYTES);
        drop(maximum_response);

        maximum_content.push(0);
        assert_eq!(
            encode_clipboard_response(
                TerminalClipboardQuery::new("c", InputEndType::Bel),
                &maximum_content,
            ),
            None
        );
    }

    #[test]
    fn response_decoder_reserves_the_larger_st_terminator_at_the_exact_boundary() {
        let maximum_body_len = MAX_ATTACHED_CLIPBOARD_SEQUENCE_BYTES - b"\x1b]52;\x1b\\".len();
        assert_eq!(
            (maximum_body_len - 1) % 4,
            0,
            "the exact-boundary fixture must contain canonical base64"
        );
        let mut maximum_body = Vec::with_capacity(maximum_body_len);
        maximum_body.push(b';');
        maximum_body.extend(std::iter::repeat_n(b'A', maximum_body_len - 1));
        assert!(decode_clipboard_response_body(&maximum_body).is_some());

        maximum_body.push(b'A');
        assert_eq!(decode_clipboard_response_body(&maximum_body), None);
    }

    #[test]
    fn pane_write_decoder_preserves_the_historical_eight_mibibyte_boundary() {
        let mut maximum_payload = vec![b'A'; MAX_PANE_CLIPBOARD_SEQUENCE_BYTES];
        assert_eq!(
            decode_pane_clipboard_write_payload(&maximum_payload)
                .expect("exactly bounded pane write decodes")
                .len(),
            MAX_PANE_CLIPBOARD_SEQUENCE_BYTES / 4 * 3
        );

        maximum_payload.push(b'A');
        assert_eq!(decode_pane_clipboard_write_payload(&maximum_payload), None);
    }
}
