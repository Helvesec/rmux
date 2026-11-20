//! What the `new-pane -I` offer must require before it may be sent.
//!
//! This lives beside the crate rather than inside it so the requirement is
//! checked from the outside, exactly as a client sees it.
use rmux_proto::{
    NewPaneInputV1Request, CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
    CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1, CAPABILITY_NEW_PANE_INPUT_V1, SUPPORTED_CAPABILITIES,
};

/// The offer executes a command, creates an explicit floating pane and carries
/// the finished input. None of the three capabilities implies another, so the
/// constructed offer has to name all three.
#[test]
fn the_offer_requires_every_capability_it_uses() {
    let promised = [
        CAPABILITY_NEW_PANE_INPUT_V1,
        CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
        CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1,
    ];
    let request = NewPaneInputV1Request::new(vec!["new-pane".to_owned()], None, None);
    assert_eq!(request.handshake.required_capabilities, promised);
    for capability in promised {
        assert!(
            SUPPORTED_CAPABILITIES.contains(&capability),
            "{capability} is advertised by this build"
        );
    }
}
