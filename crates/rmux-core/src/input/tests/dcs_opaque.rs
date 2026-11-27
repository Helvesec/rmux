use super::*;

const PACKET: &[u8] = b"\x1bPtmux;\x1b\x1b]0;outer\x07X\x1b\\";

fn assert_full_opaque_payload(writer: &RecordingWriter) {
    let payloads: Vec<_> = writer
        .calls
        .iter()
        .filter(|call| call.starts_with("dcs_passthrough("))
        .collect();
    assert_eq!(
        payloads,
        vec!["dcs_passthrough(\"\\u{1b}]0;outer\\u{7}X\")"],
        "the outer ST must dispatch exactly one complete opaque payload"
    );
    assert!(writer.chars.is_empty(), "opaque bytes reached the grid");
    assert!(!writer.has_call("set_title("));
}

#[test]
fn valid_tmux_dcs_keeps_inner_bel_opaque_whole() {
    let (parser, writer) = parse(PACKET);
    assert_eq!(parser.state(), InputState::Ground);
    assert_full_opaque_payload(&writer);
}

#[test]
fn valid_tmux_dcs_keeps_inner_bel_opaque_split_after_bel() {
    let split = PACKET.iter().position(|byte| *byte == 0x07).unwrap() + 1;
    let mut parser = InputParser::new();
    let mut writer = RecordingWriter::new(80, 24);
    parser.parse(&PACKET[..split], &mut writer);
    assert_eq!(parser.state(), InputState::DcsHandler);
    assert!(!writer.has_call("dcs_passthrough("));
    assert!(writer.chars.is_empty());

    parser.parse(&PACKET[split..], &mut writer);
    assert_eq!(parser.state(), InputState::Ground);
    assert_full_opaque_payload(&writer);
}
