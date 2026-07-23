use rmux_core::{PaneGeometry, TerminalPassthrough, TerminalPassthroughKind};

use super::types::OpenAttachTarget;

pub(super) fn render_passthroughs(
    target: &OpenAttachTarget,
    passthroughs: &[TerminalPassthrough],
) -> Vec<u8> {
    if passthroughs.is_empty() {
        return Vec::new();
    }

    let mut frame = Vec::new();
    let mut saved_cursor = false;
    for passthrough in passthroughs {
        if passthrough.clipboard_query_metadata().is_some() {
            continue;
        }
        if !passthrough_enabled(target, passthrough.kind()) {
            continue;
        }
        // Drop malformed OSC 52 writes rather than forward them verbatim to
        // the outer — tmux 3.7b's input.c input_osc_52 validates first and a
        // failed b64_pton returns before both paste_add and the outer echo.
        // A raw copy would otherwise trip xterm allowWindowOps parsers with
        // attacker-supplied payloads.
        if passthrough.kind() == TerminalPassthroughKind::Clipboard
            && !osc52_passthrough_is_valid_for_forward(passthrough)
        {
            continue;
        }
        if passthrough_requires_cursor_position(passthrough.kind()) && !saved_cursor {
            frame.extend_from_slice(b"\x1b[s");
            saved_cursor = true;
        }
        if passthrough_requires_cursor_position(passthrough.kind()) {
            append_cursor_position(&mut frame, target.active_pane_geometry, passthrough);
        }
        frame.extend_from_slice(&passthrough.render_sequence());
    }
    if saved_cursor {
        frame.extend_from_slice(b"\x1b[u");
    }
    frame
}

fn passthrough_enabled(target: &OpenAttachTarget, kind: TerminalPassthroughKind) -> bool {
    match kind {
        TerminalPassthroughKind::Raw => target.raw_passthrough,
        TerminalPassthroughKind::Clipboard => target.outer_terminal.clipboard_passthrough_enabled(),
        // This is a strict, parser-produced OSC 4 query for a u8 palette
        // index, not arbitrary raw passthrough. It must reach the outer
        // terminal even when allow-passthrough is disabled.
        TerminalPassthroughKind::PaletteQuery => true,
        TerminalPassthroughKind::KittyGraphics => target.kitty_graphics_passthrough,
        TerminalPassthroughKind::Sixel => target.sixel_passthrough,
    }
}

/// A clipboard passthrough is safe to forward if it is either a query (`?`) —
/// which many outers answer and rmux does not synthesize on its own — or a
/// well-formed OSC 52 write whose base64 payload decodes. Anything else is
/// dropped so a hostile payload cannot round-trip through the outer's OSC
/// parser under an allowWindowOps quirk.
fn osc52_passthrough_is_valid_for_forward(passthrough: &TerminalPassthrough) -> bool {
    let Some(body) = passthrough.payload().strip_prefix(b"\x1b]52;") else {
        return false;
    };
    let Some(body) = body
        .strip_suffix(b"\x07")
        .or_else(|| body.strip_suffix(b"\x1b\\"))
    else {
        return false;
    };
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return false;
    };
    let payload = &body[separator + 1..];
    if payload == b"?" {
        return true;
    }
    if payload.is_empty() {
        return false;
    }
    super::reader::osc52_payload_decodes(payload)
}

fn passthrough_requires_cursor_position(kind: TerminalPassthroughKind) -> bool {
    match kind {
        TerminalPassthroughKind::Raw
        | TerminalPassthroughKind::KittyGraphics
        | TerminalPassthroughKind::Sixel => true,
        TerminalPassthroughKind::Clipboard | TerminalPassthroughKind::PaletteQuery => false,
    }
}

fn append_cursor_position(
    frame: &mut Vec<u8>,
    geometry: PaneGeometry,
    passthrough: &TerminalPassthrough,
) {
    let row = u32::from(geometry.y())
        .saturating_add(passthrough.cursor_y())
        .saturating_add(1);
    let col = u32::from(geometry.x())
        .saturating_add(passthrough.cursor_x())
        .saturating_add(1);
    frame.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
}

#[cfg(test)]
mod tests {
    use rmux_core::{OptionStore, PaneGeometry, TerminalPaletteIndex, TerminalPassthrough};
    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode};

    use super::{append_cursor_position, render_passthroughs};
    use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
    use crate::pane_io::pane_output_channel;

    use super::super::types::OpenAttachTarget;

    #[test]
    fn cursor_position_is_absolute_and_one_based() {
        let mut frame = Vec::new();
        append_cursor_position(
            &mut frame,
            PaneGeometry::new(10, 4, 80, 24),
            &TerminalPassthrough::kitty_graphics(2, 3, b"Gf=100;AAAA".to_vec()),
        );
        assert_eq!(frame, b"\x1b[8;13H");
    }

    #[test]
    fn render_passthroughs_wraps_kitty_apc_at_pane_cursor() {
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(5, 6, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: true,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::kitty_graphics(
                1,
                2,
                b"Gf=100;AAAA".to_vec(),
            )],
        );
        assert_eq!(frame, b"\x1b[s\x1b[9;7H\x1b_Gf=100;AAAA\x1b\\\x1b[u");
    }

    #[test]
    fn render_passthroughs_anchors_kitty_dimension_payloads_at_pane_cursor() {
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(5, 6, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: true,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::kitty_graphics(
                1,
                2,
                b"Ga=p,r=10,c=20;AAAA".to_vec(),
            )],
        );
        assert_eq!(
            frame,
            b"\x1b[s\x1b[9;7H\x1b_Ga=p,r=10,c=20;AAAA\x1b\\\x1b[u"
        );
    }

    #[test]
    fn render_passthroughs_wraps_sixel_dcs_at_pane_cursor() {
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::from_pairs(&[("TERM", "foot")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(5, 6, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: true,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::sixel(1, 2, b"q#0!10~".to_vec())],
        );
        assert_eq!(frame, b"\x1b[s\x1b[9;7H\x1bPq#0!10~\x1b\\\x1b[u");
    }

    #[test]
    fn render_passthroughs_wraps_raw_payload_at_pane_cursor() {
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(5, 6, 80, 24),
            raw_passthrough: true,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::raw(
                1,
                2,
                b"\x1b]52;c;QQ==\x07".to_vec(),
            )],
        );
        assert_eq!(frame, b"\x1b[s\x1b[9;7H\x1b]52;c;QQ==\x07\x1b[u");
    }

    #[test]
    fn render_passthroughs_forwards_clipboard_without_cursor_motion() {
        let mut options = OptionStore::new();
        // Relaying an application's inbound OSC 52 to the outer requires
        // `set-clipboard on`; tmux gates it on set-clipboard == 2 and no longer
        // forwards under the `external` default (input.c input_osc_52).
        options
            .set(
                ScopeSelector::Global,
                OptionName::SetClipboard,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set-clipboard set succeeds");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &options,
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(5, 6, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::clipboard(
                b"\x1b]52;c;QQ==\x07".to_vec(),
            )],
        );
        assert_eq!(frame, b"\x1b]52;c;QQ==\x07");

        // Malformed and empty OSC 52 writes must be dropped (validate-then-drop),
        // matching tmux 3.7b's input.c input_osc_52. Under set-clipboard on the
        // valid write above is forwarded, so use the same target here.
        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::clipboard(
                b"\x1b]52;c;!!!\x07".to_vec(),
            )],
        );
        assert!(
            frame.is_empty(),
            "malformed OSC 52 write must not be forwarded: {frame:?}"
        );
        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::clipboard(b"\x1b]52;c;\x07".to_vec())],
        );
        assert!(
            frame.is_empty(),
            "empty OSC 52 payload must not be forwarded: {frame:?}"
        );
        // Typed pane-side queries are consumed by get-clipboard and must not
        // leak through this generic passthrough renderer.
        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::clipboard_query(
                rmux_core::TerminalClipboardQuery::new("c", rmux_core::input::InputEndType::Bel),
                b"\x1b]52;c;?\x07".to_vec(),
            )],
        );
        assert!(frame.is_empty());
    }

    #[test]
    fn render_passthroughs_forwards_typed_palette_query_without_raw_gate_or_cursor_motion() {
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(5, 6, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::palette_query(
                TerminalPaletteIndex::from(7),
            )],
        );

        assert_eq!(frame, b"\x1b]4;7;?\x1b\\");
    }

    #[test]
    fn render_passthroughs_is_empty_when_target_disables_passthrough() {
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(5, 6, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: false,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[TerminalPassthrough::kitty_graphics(
                1,
                2,
                b"Gf=100;AAAA".to_vec(),
            )],
        );
        assert!(frame.is_empty());
    }

    #[test]
    fn render_passthroughs_filters_by_protocol_support() {
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &OptionStore::default(),
                OuterTerminalContext::from_pairs(&[("TERM", "foot")]),
            ),
            cursor_style: 0,
            active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
            raw_passthrough: false,
            kitty_graphics_passthrough: false,
            sixel_passthrough: true,
            persistent_overlay_state_id: None,
            live_pane: None,
            render_stream: false,
        };

        let frame = render_passthroughs(
            &target,
            &[
                TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;AAAA".to_vec()),
                TerminalPassthrough::sixel(0, 1, b"q#0!10~".to_vec()),
            ],
        );
        assert_eq!(frame, b"\x1b[s\x1b[2;1H\x1bPq#0!10~\x1b\\\x1b[u");
    }
}
