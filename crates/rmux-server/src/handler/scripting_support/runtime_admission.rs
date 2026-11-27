//! Operation admission, independent of command execution and target identity.

use rmux_core::command_parser::ParsedCommand;
use rmux_proto::{HandshakeRequest, RmuxError, RuntimeCommandIntent};

use super::runtime_execution::resize_pane_requests_position;

#[derive(Clone, Copy)]
pub(super) enum RuntimeAdmission {
    /// Preserve all three C2S131 markers, including contradictory combinations.
    Legacy(OperationMarkers),
    Declared(RuntimeCommandIntent),
}

#[derive(Clone, Copy)]
pub(super) struct OperationMarkers {
    new_pane: bool,
    resize_position: bool,
    set_pane_placement: bool,
}

impl RuntimeAdmission {
    pub(super) fn legacy(handshake: &HandshakeRequest) -> Self {
        let requires = |capability| {
            handshake
                .required_capabilities
                .iter()
                .any(|cap| cap == capability)
        };
        Self::Legacy(OperationMarkers {
            new_pane: requires(rmux_proto::CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1),
            resize_position: requires(rmux_proto::CAPABILITY_RESIZE_PANE_POSITION_V1),
            set_pane_placement: requires(rmux_proto::CAPABILITY_SET_PANE_PLACEMENT_V1),
        })
    }

    pub(super) fn validate_support(self, handshake: &HandshakeRequest) -> Result<(), RmuxError> {
        if let Self::Declared(intent) = self {
            if let Some(required) = intent.required_capability() {
                if !handshake
                    .required_capabilities
                    .iter()
                    .any(|cap| cap == required)
                {
                    return Err(RmuxError::Server(format!(
                        "missing declared operation capability: {required}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn markers(self) -> OperationMarkers {
        match self {
            Self::Legacy(markers) => markers,
            Self::Declared(intent) => OperationMarkers {
                new_pane: intent == RuntimeCommandIntent::NewFloatingPane,
                resize_position: intent == RuntimeCommandIntent::MoveFloatingPane,
                set_pane_placement: intent == RuntimeCommandIntent::SetPanePlacement,
            },
        }
    }

    pub(super) fn explicit(self) -> bool {
        let markers = self.markers();
        markers.new_pane || markers.resize_position || markers.set_pane_placement
    }

    pub(super) fn requires_single_command(self) -> bool {
        matches!(self, Self::Declared(_)) && self.explicit()
    }

    pub(super) fn validate_command(self, command: &ParsedCommand) -> Result<(), RmuxError> {
        let new_pane = command.name() == "new-pane";
        let resize_position = resize_pane_requests_position(command);
        let set_pane_placement = command.name() == "set-pane-placement";
        let requires = self.markers();
        if new_pane != requires.new_pane
            || resize_position != requires.resize_position
            || set_pane_placement != requires.set_pane_placement
        {
            // Preserve C2S131's diagnostic priority for every marker combination.
            return Err(RmuxError::Server(
                if new_pane || requires.new_pane {
                    "new-pane requires an explicit floating creation offer"
                } else if resize_position || requires.resize_position {
                    "resize-pane -X/-Y requires an explicit position offer"
                } else {
                    "set-pane-placement requires an explicit placement offer"
                }
                .to_owned(),
            ));
        }
        Ok(())
    }
}
