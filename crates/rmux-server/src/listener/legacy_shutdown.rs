use rmux_proto::RMUX_FRAME_MAGIC;

const LEGACY_FRAME_HEADER_LENGTH: usize = 6;
const EMPTY_ENUM_PAYLOAD_LENGTH: usize = 4;
const LEGACY_KILL_SERVER_FRAME_LENGTH: usize =
    LEGACY_FRAME_HEADER_LENGTH + EMPTY_ENUM_PAYLOAD_LENGTH;
const LEGACY_KILL_SERVER_REQUEST_TAG: u32 = 72;
const LEGACY_KILL_SERVER_RESPONSE_TAG: u32 = 63;

/// A detached wire version shipped before the 0.9 hard cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PublishedLegacyWireVersion(u8);

impl PublishedLegacyWireVersion {
    const MINIMUM: u8 = 1;
    const MAXIMUM: u8 = 3;

    fn parse(version: u8) -> Option<Self> {
        (Self::MINIMUM..=Self::MAXIMUM)
            .contains(&version)
            .then_some(Self(version))
    }

    fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LegacyKillServerFrame {
    NotLegacyKillServer,
    Incomplete,
    Complete(PublishedLegacyWireVersion),
}

/// Recognizes only the zero-sized `KillServer` request layouts shipped on
/// detached wires 1, 2, and 3. All other frames remain owned by the ordinary
/// exact-version decoder.
pub(super) fn inspect_legacy_kill_server_frame(input: &[u8]) -> LegacyKillServerFrame {
    let Some((&magic, remaining)) = input.split_first() else {
        return LegacyKillServerFrame::NotLegacyKillServer;
    };
    if magic != RMUX_FRAME_MAGIC {
        return LegacyKillServerFrame::NotLegacyKillServer;
    }
    let Some((&wire_version, _)) = remaining.split_first() else {
        return LegacyKillServerFrame::NotLegacyKillServer;
    };
    let Some(wire_version) = PublishedLegacyWireVersion::parse(wire_version) else {
        return LegacyKillServerFrame::NotLegacyKillServer;
    };
    if input.len() < LEGACY_FRAME_HEADER_LENGTH {
        return LegacyKillServerFrame::Incomplete;
    }

    let payload_length = u32::from_le_bytes(
        input[2..LEGACY_FRAME_HEADER_LENGTH]
            .try_into()
            .expect("legacy frame header length is fixed"),
    ) as usize;
    if payload_length != EMPTY_ENUM_PAYLOAD_LENGTH {
        return LegacyKillServerFrame::NotLegacyKillServer;
    }
    if input.len() < LEGACY_KILL_SERVER_FRAME_LENGTH {
        return LegacyKillServerFrame::Incomplete;
    }
    if input.len() != LEGACY_KILL_SERVER_FRAME_LENGTH {
        return LegacyKillServerFrame::NotLegacyKillServer;
    }

    let request_tag = u32::from_le_bytes(
        input[LEGACY_FRAME_HEADER_LENGTH..LEGACY_KILL_SERVER_FRAME_LENGTH]
            .try_into()
            .expect("legacy kill-server payload length is fixed"),
    );
    if request_tag != LEGACY_KILL_SERVER_REQUEST_TAG {
        return LegacyKillServerFrame::NotLegacyKillServer;
    }

    LegacyKillServerFrame::Complete(wire_version)
}

pub(super) fn encode_legacy_kill_server_response(
    wire_version: PublishedLegacyWireVersion,
) -> [u8; LEGACY_KILL_SERVER_FRAME_LENGTH] {
    let mut frame = [0_u8; LEGACY_KILL_SERVER_FRAME_LENGTH];
    frame[0] = RMUX_FRAME_MAGIC;
    frame[1] = wire_version.get();
    frame[2..LEGACY_FRAME_HEADER_LENGTH]
        .copy_from_slice(&(EMPTY_ENUM_PAYLOAD_LENGTH as u32).to_le_bytes());
    frame[LEGACY_FRAME_HEADER_LENGTH..]
        .copy_from_slice(&LEGACY_KILL_SERVER_RESPONSE_TAG.to_le_bytes());
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_frame(wire_version: u8) -> Vec<u8> {
        let mut frame = vec![RMUX_FRAME_MAGIC, wire_version];
        frame.extend_from_slice(&(EMPTY_ENUM_PAYLOAD_LENGTH as u32).to_le_bytes());
        frame.extend_from_slice(&LEGACY_KILL_SERVER_REQUEST_TAG.to_le_bytes());
        frame
    }

    #[test]
    fn recognizes_every_published_pre_0_9_kill_server_frame() {
        for wire_version in 1..=3 {
            assert_eq!(
                inspect_legacy_kill_server_frame(&request_frame(wire_version)),
                LegacyKillServerFrame::Complete(PublishedLegacyWireVersion(wire_version))
            );
        }
    }

    #[test]
    fn waits_for_a_partial_published_legacy_kill_server_frame() {
        let frame = request_frame(3);
        for length in 2..frame.len() {
            assert_eq!(
                inspect_legacy_kill_server_frame(&frame[..length]),
                LegacyKillServerFrame::Incomplete,
                "partial length {length}"
            );
        }
    }

    #[test]
    fn rejects_unpublished_or_non_minimal_wire_versions() {
        for wire_version in [0, 4, 5, 6, 0x83] {
            assert_eq!(
                inspect_legacy_kill_server_frame(&request_frame(wire_version)),
                LegacyKillServerFrame::NotLegacyKillServer,
                "wire version byte {wire_version:#x}"
            );
        }
    }

    #[test]
    fn rejects_other_requests_and_any_trailing_bytes() {
        let mut other_request = request_frame(3);
        other_request[LEGACY_FRAME_HEADER_LENGTH..]
            .copy_from_slice(&(LEGACY_KILL_SERVER_REQUEST_TAG - 1).to_le_bytes());
        assert_eq!(
            inspect_legacy_kill_server_frame(&other_request),
            LegacyKillServerFrame::NotLegacyKillServer
        );

        let mut trailing_payload = request_frame(3);
        trailing_payload[2..LEGACY_FRAME_HEADER_LENGTH].copy_from_slice(&5_u32.to_le_bytes());
        trailing_payload.push(0);
        assert_eq!(
            inspect_legacy_kill_server_frame(&trailing_payload),
            LegacyKillServerFrame::NotLegacyKillServer
        );

        let mut trailing_frame_bytes = request_frame(3);
        trailing_frame_bytes.push(0);
        assert_eq!(
            inspect_legacy_kill_server_frame(&trailing_frame_bytes),
            LegacyKillServerFrame::NotLegacyKillServer
        );
    }

    #[test]
    fn response_uses_the_published_zero_sized_kill_server_layout() {
        for wire_version in 1..=3 {
            let version = PublishedLegacyWireVersion::parse(wire_version).expect("published wire");
            let response = encode_legacy_kill_server_response(version);
            assert_eq!(response[0], RMUX_FRAME_MAGIC);
            assert_eq!(response[1], wire_version);
            assert_eq!(
                &response[2..LEGACY_FRAME_HEADER_LENGTH],
                &(EMPTY_ENUM_PAYLOAD_LENGTH as u32).to_le_bytes()
            );
            assert_eq!(
                &response[LEGACY_FRAME_HEADER_LENGTH..],
                &LEGACY_KILL_SERVER_RESPONSE_TAG.to_le_bytes()
            );
        }
    }
}
