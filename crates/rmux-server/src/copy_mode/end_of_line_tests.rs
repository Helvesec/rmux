use super::*;
use rmux_core::input::InputParser;

#[test]
fn end_of_line_cursor_and_selection_match_tmux_37b() {
    // Measured with tmux 3.7b in an 80x24 pane: history-top,
    // start-of-line, begin-selection, end-of-line, copy-selection-and-cancel.
    // In vi the endpoint is inclusive; emacs stops one cell beyond it.
    for mode_keys in [ModeKeys::Vi, ModeKeys::Emacs] {
        for (label, line, following, vi_x, y) in [
            (
                "shorter",
                "L19-abcdefghij".to_owned(),
                "LONGER-LINE-xyz",
                13,
                0,
            ),
            ("longer", "LONGER-LINE-xyz".to_owned(), "short", 14, 0),
            ("full", "X".repeat(80), "short", 79, 0),
            ("wrapped", "W".repeat(85), "short", 4, 1),
            ("trailing spaces", "alpha   ".to_owned(), "short", 4, 0),
            ("wide", "alpha界".to_owned(), "short", 6, 0),
        ] {
            let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 200);
            InputParser::new().parse(format!("{line}\r\n{following}\r\n").as_bytes(), &mut screen);
            let context = CopyModeCommandContext {
                mode_keys,
                ..tests::test_context()
            };
            let mut state = CopyModeState::new(screen, None, false, &context, false, true);
            for command in [
                "history-top",
                "start-of-line",
                "begin-selection",
                "end-of-line",
            ] {
                state.execute_command(command, &[], &context).unwrap();
            }
            let expected_x = vi_x + u32::from(mode_keys == ModeKeys::Emacs);
            assert_eq!(
                (state.summary().cursor_x, state.summary().cursor_y),
                (expected_x, y),
                "{mode_keys:?} {label}"
            );
            let outcome = state
                .execute_command("copy-selection-and-cancel", &[], &context)
                .unwrap();
            assert_eq!(
                outcome.transfer.unwrap().data,
                line.trim_end().as_bytes(),
                "{mode_keys:?} {label}"
            );
        }
    }
}
