//! v1 frame-kind compatibility ledger.
//!
//! `FrameKind(u16)` is the v1 compatibility ledger entry identifier. It is
//! deliberately decoupled from the Rust source order of [`crate::Request`] and
//! [`crate::Response`]: each ledger entry pins a stable `u16` for a specific
//! DTO type and direction so that future re-orderings of the Rust enums do
//! not silently change wire identity.
//!
//! The current wire format still encodes DTOs through `bincode_v1`. The
//! ledger therefore additionally pins each variant's bincode tag, and a
//! companion test in `tests/wire_ledger_v1.rs` enforces that every active
//! ledger entry's `FrameKind` low bits match the bincode tag observed for a
//! sample value of the corresponding variant.
//!
//! Numeric layout:
//! - `0x0000..=0x7FFF` is reserved for client→server frames.
//! - `0x8000..=0xFFFF` is reserved for server→client frames.
//! - The high bit (`0x8000`) therefore acts as a direction tag and the low
//!   15 bits mirror the bincode tag of the matching variant.
//!
//! Removed or future values stay listed with [`FrameStatus::Reserved`] and are
//! never reused. Adding a new variant requires both an enum entry and a
//! ledger entry; removing a variant requires marking the ledger entry
//! `Reserved` rather than deleting it.

use crate::{Request, Response};

mod request_commands;
mod request_control;
mod request_sdk;
mod response_commands;
mod response_control;
mod response_sdk;
mod vocabulary;
pub use vocabulary::{FrameDirection, FrameFeature, FrameKind, FrameLedgerEntry, FrameStatus};

const DIRECTION_BIT: u16 = 0x8000;

const fn c2s(tag: u16) -> FrameKind {
    FrameKind(tag)
}

const fn s2c(tag: u16) -> FrameKind {
    FrameKind(DIRECTION_BIT | tag)
}

const fn entry(
    kind: FrameKind,
    direction: FrameDirection,
    status: FrameStatus,
    dto_type: &'static str,
    feature: FrameFeature,
    fixture: Option<&'static str>,
    compatibility_note: &'static str,
) -> FrameLedgerEntry {
    FrameLedgerEntry {
        kind,
        direction,
        status,
        dto_type,
        feature,
        fixture,
        compatibility_note,
    }
}

const ACTIVE: FrameStatus = FrameStatus::Active;
const RESERVED: FrameStatus = FrameStatus::Reserved;

/// Authoritative v1 ledger.
///
/// Entries are sorted by `(direction, kind)`.
pub const V1_FRAME_LEDGER: &[FrameLedgerEntry] = &assemble_ledger();

// Preserve wire order while keeping each direction's command families together.
const LEDGER_SECTIONS: &[&[FrameLedgerEntry]] = &[
    request_commands::ENTRIES,
    request_control::ENTRIES,
    request_sdk::ENTRIES,
    response_commands::ENTRIES,
    response_control::ENTRIES,
    response_sdk::ENTRIES,
];

const fn ledger_len() -> usize {
    let mut len = 0;
    let mut section = 0;
    while section < LEDGER_SECTIONS.len() {
        len += LEDGER_SECTIONS[section].len();
        section += 1;
    }
    len
}

const fn assemble_ledger() -> [FrameLedgerEntry; ledger_len()] {
    let mut entries = [request_commands::ENTRIES[0]; ledger_len()];
    let mut section = 0;
    let mut offset = 0;
    while section < LEDGER_SECTIONS.len() {
        let source = LEDGER_SECTIONS[section];
        let mut index = 0;
        while index < source.len() {
            entries[offset] = source[index];
            offset += 1;
            index += 1;
        }
        section += 1;
    }
    entries
}

/// Returns the recorded `FrameKind` for a `Request` value.
///
/// The match is exhaustive on the v1 `Request` variants. Reordering or
/// renaming variants requires updating both this match and the corresponding
/// ledger entry.
#[must_use]
pub const fn frame_kind_for_request(request: &Request) -> FrameKind {
    match request {
        Request::NewSession(_) => c2s(0),
        Request::HasSession(_) => c2s(1),
        Request::KillSession(_) => c2s(2),
        Request::NewWindow(_) => c2s(3),
        Request::KillWindow(_) => c2s(4),
        Request::SelectWindow(_) => c2s(5),
        Request::RenameWindow(_) => c2s(6),
        Request::NextWindow(_) => c2s(7),
        Request::PreviousWindow(_) => c2s(8),
        Request::LastWindow(_) => c2s(9),
        Request::ListWindows(_) => c2s(10),
        Request::MoveWindow(_) => c2s(11),
        Request::SwapWindow(_) => c2s(12),
        Request::RotateWindow(_) => c2s(13),
        Request::SplitWindow(_) => c2s(14),
        Request::SwapPane(_) => c2s(15),
        Request::LastPane(_) => c2s(16),
        Request::JoinPane(_) => c2s(17),
        Request::BreakPane(_) => c2s(18),
        Request::KillPane(_) => c2s(19),
        Request::SelectLayout(_) => c2s(20),
        Request::ResizePane(_) => c2s(21),
        Request::DisplayPanes(_) => c2s(22),
        Request::SelectPane(_) => c2s(23),
        Request::SelectPaneAdjacent(_) => c2s(24),
        Request::SendKeys(_) => c2s(25),
        Request::AttachSession(_) => c2s(26),
        Request::SwitchClient(_) => c2s(27),
        Request::DetachClient(_) => c2s(28),
        Request::SetOption(_) => c2s(29),
        Request::SetEnvironment(_) => c2s(30),
        Request::SetHook(_) => c2s(31),
        Request::NextLayout(_) => c2s(32),
        Request::PreviousLayout(_) => c2s(33),
        Request::ShowOptions(_) => c2s(34),
        Request::ShowEnvironment(_) => c2s(35),
        Request::SetBuffer(_) => c2s(36),
        Request::ShowBuffer(_) => c2s(37),
        Request::PasteBuffer(_) => c2s(38),
        Request::ListBuffers(_) => c2s(39),
        Request::DeleteBuffer(_) => c2s(40),
        Request::LoadBuffer(_) => c2s(41),
        Request::SaveBuffer(_) => c2s(42),
        Request::CapturePane(_) => c2s(43),
        Request::DisplayMessage(_) => c2s(44),
        Request::RunShell(_) => c2s(45),
        Request::IfShell(_) => c2s(46),
        Request::WaitFor(_) => c2s(47),
        Request::RenameSession(_) => c2s(48),
        Request::ListSessions(_) => c2s(49),
        Request::ListPanes(_) => c2s(50),
        Request::SourceFile(_) => c2s(51),
        Request::SetOptionByName(_) => c2s(52),
        Request::SetHookMutation(_) => c2s(53),
        Request::ShowHooks(_) => c2s(54),
        Request::SendKeysExt(_) => c2s(55),
        Request::SwitchClientExt(_) => c2s(56),
        Request::BindKey(_) => c2s(57),
        Request::UnbindKey(_) => c2s(58),
        Request::ListKeys(_) => c2s(59),
        Request::SendPrefix(_) => c2s(60),
        Request::ClearHistory(_) => c2s(61),
        Request::CopyMode(_) => c2s(62),
        Request::ControlMode(_) => c2s(63),
        Request::ClockMode(_) => c2s(64),
        Request::ShowMessages(_) => c2s(65),
        Request::NewSessionExt(_) => c2s(66),
        Request::AttachSessionExt(_) => c2s(67),
        Request::SwitchClientExt2(_) => c2s(68),
        Request::SelectCustomLayout(_) => c2s(69),
        Request::SelectOldLayout(_) => c2s(70),
        Request::SpreadLayout(_) => c2s(71),
        Request::KillServer(_) => c2s(72),
        Request::LockServer(_) => c2s(73),
        Request::LockSession(_) => c2s(74),
        Request::LockClient(_) => c2s(75),
        Request::ServerAccess(_) => c2s(76),
        Request::RefreshClient(_) => c2s(77),
        Request::ListClients(_) => c2s(78),
        Request::SuspendClient(_) => c2s(79),
        Request::DetachClientExt(_) => c2s(80),
        Request::AttachSessionExt2(_) => c2s(81),
        Request::SwitchClientExt3(_) => c2s(82),
        Request::ResizeWindow(_) => c2s(83),
        Request::RespawnWindow(_) => c2s(84),
        Request::MovePane(_) => c2s(85),
        Request::PipePane(_) => c2s(86),
        Request::RespawnPane(_) => c2s(87),
        Request::LinkWindow(_) => c2s(88),
        Request::UnlinkWindow(_) => c2s(89),
        Request::SelectPaneMark(_) => c2s(90),
        Request::ResolveTarget(_) => c2s(91),
        Request::SplitWindowExt(_) => c2s(92),
        Request::Handshake(_) => c2s(93),
        Request::PaneSnapshot(_) => c2s(94),
        Request::SubscribePaneOutput(_) => c2s(95),
        Request::UnsubscribePaneOutput(_) => c2s(96),
        Request::PaneOutputCursor(_) => c2s(97),
        Request::SdkWaitForOutput(_) => c2s(98),
        Request::CancelSdkWait(_) => c2s(99),
        Request::PaneInput(_) => c2s(100),
        Request::PaneResize(_) => c2s(101),
        Request::PaneKill(_) => c2s(102),
        Request::PaneRespawn(_) => c2s(103),
        Request::PaneSnapshotRef(_) => c2s(104),
        Request::PaneSelect(_) => c2s(105),
        Request::PaneBroadcastInput(_) => c2s(106),
        Request::CreateSessionLease(_) => c2s(107),
        Request::RenewSessionLease(_) => c2s(108),
        Request::ReleaseSessionLease(_) => c2s(109),
        Request::SubscribePaneOutputRef(_) => c2s(110),
        Request::SdkWaitForOutputRef(_) => c2s(111),
        Request::DaemonStatus(_) => c2s(112),
        Request::ShutdownIfIdle(_) => c2s(113),
        Request::WebShare(_) => c2s(114),
        Request::DisplayMessageExt(_) => c2s(115),
        Request::SendKeysExt2(_) => c2s(116),
        Request::AttachSessionExt3(_) => c2s(117),
        Request::SplitWindowTargetAction(_) => c2s(118),
        Request::ResizePaneTargetAction(_) => c2s(119),
        Request::CapturePaneTargetAction(_) => c2s(120),
        Request::PaneOptionSet(_) => c2s(121),
        Request::PaneOptionGet(_) => c2s(122),
        Request::SubscribePaneState(_) => c2s(123),
        Request::PaneStateCursor(_) => c2s(124),
        Request::UnsubscribePaneState(_) => c2s(125),
        Request::PaneForegroundState(_) => c2s(126),
        Request::SplitWindowIdentity(_) => c2s(127),
        Request::SubscribePaneStream(_) => c2s(128),
        Request::PaneStreamCursor(_) => c2s(129),
        Request::UnsubscribePaneStream(_) => c2s(130),
        Request::RuntimeCommandExecution(_) => c2s(131),
        Request::RuntimeCommandExecutionV2(_) => c2s(135),
        Request::SplitWindowCommandV1(_) => c2s(132),
        Request::NewPaneInputV1(_) => c2s(136),
        Request::SourceFileInputV1(_) => c2s(140),
        Request::ResizePaneAxes(_) => c2s(139),
        Request::ExportWindowSnapshotV2(_) => c2s(137),
        Request::RestoreWindowSnapshotV2(_) => c2s(138),
        Request::ExportWindowSnapshot(_) => c2s(133),
        Request::RestoreWindowSnapshot(_) => c2s(134),
    }
}

/// Returns the recorded `FrameKind` for a `Response` value.
#[must_use]
pub const fn frame_kind_for_response(response: &Response) -> FrameKind {
    match response {
        Response::NewSession(_) => s2c(0),
        Response::HasSession(_) => s2c(1),
        Response::KillSession(_) => s2c(2),
        Response::NewWindow(_) => s2c(3),
        Response::KillWindow(_) => s2c(4),
        Response::SelectWindow(_) => s2c(5),
        Response::RenameWindow(_) => s2c(6),
        Response::NextWindow(_) => s2c(7),
        Response::PreviousWindow(_) => s2c(8),
        Response::LastWindow(_) => s2c(9),
        Response::ListWindows(_) => s2c(10),
        Response::MoveWindow(_) => s2c(11),
        Response::SwapWindow(_) => s2c(12),
        Response::RotateWindow(_) => s2c(13),
        Response::SplitWindow(_) => s2c(14),
        Response::SwapPane(_) => s2c(15),
        Response::LastPane(_) => s2c(16),
        Response::JoinPane(_) => s2c(17),
        Response::BreakPane(_) => s2c(18),
        Response::KillPane(_) => s2c(19),
        Response::SelectLayout(_) => s2c(20),
        Response::ResizePane(_) => s2c(21),
        Response::DisplayPanes(_) => s2c(22),
        Response::SelectPane(_) => s2c(23),
        Response::SendKeys(_) => s2c(24),
        Response::AttachSession(_) => s2c(25),
        Response::SwitchClient(_) => s2c(26),
        Response::DetachClient(_) => s2c(27),
        Response::SetOption(_) => s2c(28),
        Response::SetEnvironment(_) => s2c(29),
        Response::SetHook(_) => s2c(30),
        Response::Error(_) => s2c(31),
        Response::NextLayout(_) => s2c(32),
        Response::PreviousLayout(_) => s2c(33),
        Response::ShowOptions(_) => s2c(34),
        Response::ShowEnvironment(_) => s2c(35),
        Response::SetBuffer(_) => s2c(36),
        Response::ShowBuffer(_) => s2c(37),
        Response::PasteBuffer(_) => s2c(38),
        Response::ListBuffers(_) => s2c(39),
        Response::DeleteBuffer(_) => s2c(40),
        Response::LoadBuffer(_) => s2c(41),
        Response::SaveBuffer(_) => s2c(42),
        Response::CapturePane(_) => s2c(43),
        Response::DisplayMessage(_) => s2c(44),
        Response::RunShell(_) => s2c(45),
        Response::IfShell(_) => s2c(46),
        Response::WaitFor(_) => s2c(47),
        Response::RenameSession(_) => s2c(48),
        Response::ListSessions(_) => s2c(49),
        Response::ListPanes(_) => s2c(50),
        Response::SourceFile(_) => s2c(51),
        Response::SetOptionByName(_) => s2c(52),
        Response::ShowHooks(_) => s2c(53),
        Response::BindKey(_) => s2c(54),
        Response::UnbindKey(_) => s2c(55),
        Response::ListKeys(_) => s2c(56),
        Response::SendPrefix(_) => s2c(57),
        Response::ClearHistory(_) => s2c(58),
        Response::CopyMode(_) => s2c(59),
        Response::ControlMode(_) => s2c(60),
        Response::ClockMode(_) => s2c(61),
        Response::ShowMessages(_) => s2c(62),
        Response::KillServer(_) => s2c(63),
        Response::LockServer(_) => s2c(64),
        Response::LockSession(_) => s2c(65),
        Response::LockClient(_) => s2c(66),
        Response::ServerAccess(_) => s2c(67),
        Response::RefreshClient(_) => s2c(68),
        Response::ListClients(_) => s2c(69),
        Response::SuspendClient(_) => s2c(70),
        Response::ResizeWindow(_) => s2c(71),
        Response::RespawnWindow(_) => s2c(72),
        Response::MovePane(_) => s2c(73),
        Response::PipePane(_) => s2c(74),
        Response::RespawnPane(_) => s2c(75),
        Response::LinkWindow(_) => s2c(76),
        Response::UnlinkWindow(_) => s2c(77),
        Response::ResolveTarget(_) => s2c(78),
        Response::Handshake(_) => s2c(79),
        Response::PaneSnapshot(_) => s2c(80),
        Response::SubscribePaneOutput(_) => s2c(81),
        Response::UnsubscribePaneOutput(_) => s2c(82),
        Response::PaneOutputCursor(_) => s2c(83),
        Response::PaneOutputLag(_) => s2c(84),
        Response::SdkWaitForOutput(_) => s2c(85),
        Response::CancelSdkWait(_) => s2c(86),
        Response::PaneBroadcastInput(_) => s2c(87),
        Response::CreateSessionLease(_) => s2c(88),
        Response::RenewSessionLease(_) => s2c(89),
        Response::ReleaseSessionLease(_) => s2c(90),
        Response::DaemonStatus(_) => s2c(91),
        Response::ShutdownIfIdle(_) => s2c(92),
        Response::WebShare(_) => s2c(93),
        Response::PaneOptionSet(_) => s2c(94),
        Response::PaneOptionGet(_) => s2c(95),
        Response::SubscribePaneState(_) => s2c(96),
        Response::PaneStateCursor(_) => s2c(97),
        Response::PaneStateLag(_) => s2c(98),
        Response::UnsubscribePaneState(_) => s2c(99),
        Response::PaneForegroundState(_) => s2c(100),
        Response::SplitWindowIdentity(_) => s2c(101),
        Response::SubscribePaneStream(_) => s2c(102),
        Response::PaneStreamCursor(_) => s2c(103),
        Response::UnsubscribePaneStream(_) => s2c(104),
        Response::RuntimeCommandExecution(_) => s2c(105),
        Response::SplitWindowCommandV1(_) => s2c(106),
        Response::NewPaneInputV1(_) => s2c(109),
        Response::ExportWindowSnapshotV2(_) => s2c(110),
        Response::RestoreWindowSnapshotV2(_) => s2c(111),
        Response::SourceFileQueueV1(_) => s2c(112),
        Response::ExportWindowSnapshot(_) => s2c(107),
        Response::RestoreWindowSnapshot(_) => s2c(108),
    }
}

/// Looks up a ledger entry by its `FrameKind` value.
#[must_use]
pub fn ledger_entry_for(kind: FrameKind) -> Option<&'static FrameLedgerEntry> {
    V1_FRAME_LEDGER.iter().find(|entry| entry.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_kinds_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in V1_FRAME_LEDGER {
            assert!(
                seen.insert(entry.kind),
                "duplicate FrameKind {:#06x} for {}",
                entry.kind.0,
                entry.dto_type
            );
        }
    }

    #[test]
    fn direction_bit_partitions_kinds() {
        for entry in V1_FRAME_LEDGER {
            assert_eq!(entry.kind.direction(), entry.direction);
        }
    }

    #[test]
    fn fixture_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for entry in V1_FRAME_LEDGER {
            if let Some(name) = entry.fixture {
                assert!(seen.insert(name), "duplicate fixture {name}");
            }
        }
    }

    #[test]
    fn reserved_entries_have_reserved_feature() {
        for entry in V1_FRAME_LEDGER {
            if matches!(entry.status, FrameStatus::Reserved) {
                assert!(matches!(entry.feature, FrameFeature::Reserved));
            }
        }
    }

    #[test]
    fn bincode_tag_never_uses_direction_bit() {
        // Edge case: any entry whose `bincode_tag()` is >= 0x8000 means a
        // direction bit leaked into the payload tag, which would corrupt
        // the bincode wire encoding for that variant.
        for entry in V1_FRAME_LEDGER {
            assert!(
                entry.kind.bincode_tag() < 0x8000,
                "ledger entry {} kind {:#06x} bincode_tag {} ≥ 0x8000",
                entry.dto_type,
                entry.kind.0,
                entry.kind.bincode_tag(),
            );
        }
    }

    #[test]
    fn ledger_entry_for_returns_each_active_entry() {
        // Edge case: every entry recorded in the ledger must be reachable
        // through the public `ledger_entry_for` lookup. Catches a future
        // change that splits the ledger into multiple slices but forgets
        // to register one of them.
        for entry in V1_FRAME_LEDGER {
            let looked_up = ledger_entry_for(entry.kind)
                .unwrap_or_else(|| panic!("kind {:#06x} not reachable", entry.kind.0));
            assert_eq!(looked_up.kind, entry.kind);
            assert_eq!(looked_up.direction, entry.direction);
            assert_eq!(looked_up.dto_type, entry.dto_type);
        }
    }

    #[test]
    fn reserved_kinds_keep_direction_bit_consistent() {
        // Reserved sentinels live near the ceiling of each direction band.
        // Their stored `kind` value's high bit must agree with their
        // recorded `direction`; otherwise a contributor could "reserve" a
        // c2s slot inside the s2c band by mistake.
        for entry in V1_FRAME_LEDGER
            .iter()
            .filter(|entry| matches!(entry.status, FrameStatus::Reserved))
        {
            let direction_bit_set = entry.kind.0 & DIRECTION_BIT != 0;
            match entry.direction {
                FrameDirection::ClientToServer => assert!(
                    !direction_bit_set,
                    "reserved c2s entry {:#06x} has direction bit set",
                    entry.kind.0
                ),
                FrameDirection::ServerToClient => assert!(
                    direction_bit_set,
                    "reserved s2c entry {:#06x} missing direction bit",
                    entry.kind.0
                ),
            }
        }
    }
}
