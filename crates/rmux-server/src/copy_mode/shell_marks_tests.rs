use super::*;
use rmux_core::TerminalScreen;
use rmux_proto::TerminalSize;

fn state(rows: u16) -> CopyModeState {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 20, rows }, 20);
    terminal.feed(b"\x1b]133;A\x07p0\r\n\x1b]133;C\x07o0\r\nx\r\n\x1b]133;A\x07p1\r\n\x1b]133;C\x07o1\r\nx\r\n\x1b]133;A\x07p2\r\nend");
    CopyModeState::for_test(terminal.screen().clone())
}

#[test]
fn shell_marks_navigation_skips_current_and_selects_output_with_o() {
    let mut state = state(8);
    for (direction, output, y) in [
        (false, false, 6),
        (false, false, 3),
        (false, false, 0),
        (false, false, 0),
        (true, false, 3),
        (true, true, 4),
        (false, true, 1),
        (false, true, 1),
        (true, true, 4),
        (true, true, 4),
        (true, false, 6),
        (true, false, 6),
    ] {
        if direction {
            state.cmd_next_prompt(output).unwrap();
        } else {
            state.cmd_previous_prompt(output).unwrap();
        }
        assert_eq!(state.cursor, CopyPosition { x: 0, y });
    }
}

#[test]
fn shell_marks_history_navigation_publishes_oracle_viewport_and_selection() {
    let mut state = state(4);
    let ctx = super::tests::test_context();
    state.execute_command("begin-selection", &[], &ctx).unwrap();
    for (command, args, expected) in [
        ("previous-prompt", vec![], (2, 0)),
        ("previous-prompt", vec![], (0, 1)),
        ("previous-prompt", vec![], (0, 4)),
        ("next-prompt", vec!["-o".to_owned()], (0, 3)),
        ("next-prompt", vec!["-o".to_owned()], (0, 0)),
    ] {
        state.execute_command(command, &args, &ctx).unwrap();
        let summary = state.summary();
        assert_eq!((summary.cursor_y, summary.scroll_position), expected);
        assert_eq!(summary.cursor_x, 0);
        assert_eq!(state.selection_snapshot().unwrap().end, state.cursor);
    }
}

#[test]
fn shell_marks_copy_snapshot_and_replacement_use_screen_metadata() {
    let mut state = state(4);
    let before = state.clone();
    let mut source = TerminalScreen::new(TerminalSize { cols: 20, rows: 4 }, 20);
    source.feed(b"unmarked\r\nlast");
    state.cmd_previous_prompt(false).unwrap();
    assert_eq!(state.cursor.y, 6);
    assert_eq!(before.cursor.y, 7);
    // New copy-mode state after a runtime replacement cannot inherit marks.
    let mut replaced = CopyModeState::for_test(source.screen().clone());
    let cursor = replaced.cursor;
    replaced.cmd_previous_prompt(false).unwrap();
    assert_eq!(replaced.cursor, cursor);
}
