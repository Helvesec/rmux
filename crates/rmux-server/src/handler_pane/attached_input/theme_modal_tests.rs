use super::{ModalPaletteInputSegment, RequestHandler, TerminalControlEvent};

#[test]
fn theme_modal_reports_and_backgrounds_survive_every_split_without_becoming_keys() {
    let stream = b"x\x1b[?997;1n\x1b]11;#ffffff\x07\x1b[?997;2n\x1b]11;#000000\x1b\\\x1b[?997;9ny";
    for split in 0..=stream.len() {
        let handler = RequestHandler::new();
        let mut pending = Vec::new();
        let mut events = Vec::new();
        let mut keys = Vec::new();
        for part in [&stream[..split], &stream[split..]] {
            match handler.split_attached_modal_palette_input(&pending, part) {
                Some(input) => {
                    pending = input.retained;
                    for segment in input.segments {
                        match segment {
                            ModalPaletteInputSegment::TerminalControl { event, .. } => {
                                events.push(event)
                            }
                            ModalPaletteInputSegment::Input(bytes) => keys.extend(bytes),
                            _ => panic!("unexpected modal protocol segment"),
                        }
                    }
                }
                None => {
                    keys.append(&mut pending);
                    keys.extend_from_slice(part);
                }
            }
        }
        assert!(pending.is_empty(), "split={split}");
        assert_eq!(keys, b"xy", "split={split}");
        assert_eq!(
            events,
            [
                TerminalControlEvent::ClientDarkTheme,
                TerminalControlEvent::ClientBackground([255; 3]),
                TerminalControlEvent::ClientLightTheme,
                TerminalControlEvent::ClientBackground([0; 3]),
            ],
            "split={split}"
        );
    }
}

#[test]
fn theme_modal_scanner_keeps_paste_opaque_and_shares_focus_control_segments() {
    let handler = RequestHandler::new();
    let paste = b"\x1b[200~\x1b[?997;1n\x1b]11;#ffffff\x07\x1b[201~";
    assert!(handler
        .split_attached_modal_palette_input(&[], paste)
        .is_none());

    let split = handler
        .split_attached_modal_palette_input(&[], b"\x1b[I\x1b[O\x1b[A")
        .expect("FOC reports share terminal-control admission before modal keys");
    assert!(split.retained.is_empty());
    let mut segments = split.segments.into_iter();
    assert!(matches!(
        segments.next(),
        Some(ModalPaletteInputSegment::TerminalControl {
            event: TerminalControlEvent::FocusIn,
            ..
        })
    ));
    assert!(matches!(
        segments.next(),
        Some(ModalPaletteInputSegment::TerminalControl {
            event: TerminalControlEvent::FocusOut,
            ..
        })
    ));
    assert!(matches!(
        segments.next(),
        Some(ModalPaletteInputSegment::Input(bytes)) if bytes == b"\x1b[A"
    ));
    assert!(segments.next().is_none());
}
