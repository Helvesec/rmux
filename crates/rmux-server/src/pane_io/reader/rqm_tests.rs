use super::{publish_pane_bytes, PanePublishContext};
use crate::{pane_io::pane_output_channel, pane_transcript::PaneTranscript};
use rmux_core::PaneId;
use rmux_proto::{SessionName, TerminalSize};

#[test]
fn rqm_publication_isolates_two_panes_and_drains_in_order() {
    let panes = [
        PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 }),
        PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 }),
    ];
    panes[0].lock().set_cursor_default_style(4);
    panes[1].lock().set_cursor_default_style(5);
    let session = SessionName::new("rqm").unwrap();
    for (index, (pane, style)) in panes.iter().zip([2, 1]).enumerate() {
        let output = pane_output_channel();
        // Both native readers obtain their reply bytes from this publication
        // path, independently of presentation demand or the active pane.
        let context = || PanePublishContext {
            session_name: &session,
            pane_id: PaneId::new(index as u32),
            transcript: pane,
            pane_output: &output,
            generation: None,
            pane_alert_callback: None,
            status_terminal: None,
            emit_no_bell_alert: false,
        };
        let replies = publish_pane_bytes(
            context(),
            b"\x1b[?2026h\x1b[?12$p\x1b[4h\x1b[4$p\x1b[?2031h\x1b[?2031$p\x1b[c".to_vec(),
        );
        assert_eq!(
            replies,
            format!("\x1b[?12;{style}$y\x1b[4;1$y\x1b[?2031;1$y\x1b[?1;2c").as_bytes()
        );
        assert!(publish_pane_bytes(context(), Vec::new()).is_empty());
    }
}
