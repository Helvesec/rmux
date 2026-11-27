//! Explicit wire-9 admission of the negotiated, additive C2S140 message.

use rmux_proto::{
    frame_kind_for_request, ledger_entry_for, FrameDirection, FrameFeature, FrameKind, FrameStatus,
    Request, SourceFileInputV1Request, SOURCE_FILE_INPUT_V1_REQUIRED_CAPABILITIES, V1_FRAME_LEDGER,
};

#[test]
fn source_file_input_c2s140_ledger_cardinality_and_content() {
    assert_eq!(
        V1_FRAME_LEDGER
            .iter()
            .filter(|entry| entry.status == FrameStatus::Active)
            .count(),
        259,
        "includes source execution, queue status, stdin pull and the three retained-reader frames"
    );
    let entry = ledger_entry_for(FrameKind(140)).expect("C2S140 is registered");
    assert_eq!(entry.kind, FrameKind(140));
    assert_eq!(entry.direction, FrameDirection::ClientToServer);
    assert_eq!(entry.status, FrameStatus::Active);
    assert_eq!(entry.dto_type, "SourceFileInputV1Request");
    assert_eq!(entry.feature, FrameFeature::Panes);
    assert_eq!(entry.fixture, None);
    assert_eq!(
        entry.compatibility_note,
        "Wire v9 (0.11.0); requires commands.source_file.input.v1, \
         commands.cli_runtime_command_execution.v1 and commands.new_pane.floating_explicit.v1 \
         per request. Runs file-backed source-file with separate binary input, consumed only \
         after successful new-pane -I creation. Without commands.source_file.input.v1, \
         CLI discovery keeps the ordinary source route without a binary channel; \
         new-pane -I refuses missing input. Refusals are terminal, without replay. \
         Existing DTOs and V1 payloads are unchanged."
    );
    assert_eq!(
        SOURCE_FILE_INPUT_V1_REQUIRED_CAPABILITIES,
        [
            "commands.source_file.input.v1",
            "commands.cli_runtime_command_execution.v1",
            "commands.new_pane.floating_explicit.v1",
        ]
    );
    let request = Request::SourceFileInputV1(Box::new(SourceFileInputV1Request::new(
        vec!["source-file".into(), "input.conf".into()],
        None,
        Some(vec![0, 255]),
    )));
    assert_eq!(frame_kind_for_request(&request), entry.kind);
    let bytes = bincode::serialize(&request).expect("request encodes");
    assert_eq!(&bytes[..4], &140_u32.to_le_bytes());
}
