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
        if !passthrough_enabled(target, passthrough.kind()) {
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
        TerminalPassthroughKind::KittyGraphics => target.kitty_graphics_passthrough,
        TerminalPassthroughKind::Sixel => target.sixel_passthrough,
    }
}

fn passthrough_requires_cursor_position(kind: TerminalPassthroughKind) -> bool {
    match kind {
        TerminalPassthroughKind::Raw
        | TerminalPassthroughKind::KittyGraphics
        | TerminalPassthroughKind::Sixel => true,
        TerminalPassthroughKind::Clipboard => false,
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
    use rmux_core::{OptionStore, PaneGeometry, TerminalPassthrough};
    use rmux_proto::{OptionName, PaneTarget, ScopeSelector, SessionName, SetOptionMode};
    use rmux_pty::PtyPair;

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
        let pty = PtyPair::open().expect("open pty pair");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            input_target: PaneTarget::with_window(
                SessionName::new("alpha").expect("valid session name"),
                0,
                0,
            ),
            pane_master: Some(pty.into_master()),
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
        let pty = PtyPair::open().expect("open pty pair");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            input_target: PaneTarget::with_window(
                SessionName::new("alpha").expect("valid session name"),
                0,
                0,
            ),
            pane_master: Some(pty.into_master()),
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
        let pty = PtyPair::open().expect("open pty pair");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            input_target: PaneTarget::with_window(
                SessionName::new("alpha").expect("valid session name"),
                0,
                0,
            ),
            pane_master: Some(pty.into_master()),
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
        let pty = PtyPair::open().expect("open pty pair");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            input_target: PaneTarget::with_window(
                SessionName::new("alpha").expect("valid session name"),
                0,
                0,
            ),
            pane_master: Some(pty.into_master()),
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
        options
            .set(
                ScopeSelector::Global,
                OptionName::SetClipboard,
                "external".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set-clipboard set succeeds");
        let pty = PtyPair::open().expect("open pty pair");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            input_target: PaneTarget::with_window(
                SessionName::new("alpha").expect("valid session name"),
                0,
                0,
            ),
            pane_master: Some(pty.into_master()),
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
    }

    #[test]
    fn render_passthroughs_is_empty_when_target_disables_passthrough() {
        let pty = PtyPair::open().expect("open pty pair");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            input_target: PaneTarget::with_window(
                SessionName::new("alpha").expect("valid session name"),
                0,
                0,
            ),
            pane_master: Some(pty.into_master()),
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
        let pty = PtyPair::open().expect("open pty pair");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name: SessionName::new("alpha").expect("valid session name"),
            input_target: PaneTarget::with_window(
                SessionName::new("alpha").expect("valid session name"),
                0,
                0,
            ),
            pane_master: Some(pty.into_master()),
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
