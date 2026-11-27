use super::*;
use rmux_core::input::InputParser;

fn selection(mode_keys: ModeKeys, content: &str, commands: &[&str]) -> CopyModeState {
    let mut screen = Screen::new(TerminalSize { cols: 80, rows: 24 }, 200);
    InputParser::new().parse(content.as_bytes(), &mut screen);
    let context = CopyModeCommandContext {
        mode_keys,
        ..tests::test_context()
    };
    let mut state = CopyModeState::new(screen, None, false, &context, false, true);
    for command in ["history-top", "start-of-line"]
        .into_iter()
        .chain(commands.iter().copied())
    {
        state.execute_command(command, &[], &context).unwrap();
    }
    state
}

#[test]
fn empty_char_selection_matches_tmux_37b() {
    // Native tmux 3.7b measurements: a vi selection includes the empty
    // final line's LF, while an emacs endpoint at column zero excludes it.
    for mode in [ModeKeys::Vi, ModeKeys::Emacs] {
        for (content, commands, vi, emacs) in [
            (
                "\r\nshort\r\n",
                vec!["begin-selection", "end-of-line"],
                "\n",
                "",
            ),
            (
                "   \r\nshort\r\n",
                vec!["begin-selection", "end-of-line"],
                "\n",
                "",
            ),
            (
                "\r\nshort\r\n",
                vec!["end-of-line", "begin-selection", "start-of-line"],
                "\n",
                "",
            ),
            (
                "abc\r\n\r\nshort\r\n",
                vec!["begin-selection", "cursor-down", "end-of-line"],
                "abc\n\n",
                "abc\n",
            ),
            (
                "\r\nabc\r\nshort\r\n",
                vec!["begin-selection", "cursor-down", "end-of-line"],
                "\nabc",
                "\nabc",
            ),
            (
                "abc\r\n\r\ndef\r\n",
                vec![
                    "begin-selection",
                    "cursor-down",
                    "cursor-down",
                    "end-of-line",
                ],
                "abc\n\ndef",
                "abc\n\ndef",
            ),
        ] {
            let mut state = selection(mode, content, &commands);
            let context = CopyModeCommandContext {
                mode_keys: mode,
                ..tests::test_context()
            };
            let outcome = state
                .execute_command("copy-selection-and-cancel", &[], &context)
                .unwrap();
            assert_eq!(
                outcome.transfer.unwrap().data,
                if mode == ModeKeys::Vi { vi } else { emacs }.as_bytes(),
                "{mode:?} {content:?} {commands:?}"
            );
        }
    }
}

#[test]
fn empty_vi_selection_reaches_every_selection_transfer() {
    let context = CopyModeCommandContext {
        mode_keys: ModeKeys::Vi,
        ..tests::test_context()
    };
    for command in [
        "copy-selection",
        "copy-selection-no-clear",
        "copy-selection-and-cancel",
        "append-selection",
        "append-selection-and-cancel",
        "copy-pipe",
        "copy-pipe-no-clear",
        "copy-pipe-and-cancel",
        "pipe",
        "pipe-no-clear",
        "pipe-and-cancel",
    ] {
        let mut state = selection(
            ModeKeys::Vi,
            "\r\nshort\r\n",
            &["begin-selection", "end-of-line"],
        );
        let outcome = state.execute_command(command, &[], &context).unwrap();
        assert_eq!(outcome.transfer.unwrap().data, b"\n", "{command}");
    }
}

#[test]
fn vi_wrapped_blank_cells_remain_content_not_line_endings() {
    // tmux 3.7b retains spaces on wrapped rows, even an entirely blank row.
    let context = CopyModeCommandContext {
        mode_keys: ModeKeys::Vi,
        ..tests::test_context()
    };
    for line in [
        format!("abc{}tail", " ".repeat(77)),
        format!("{}tail", " ".repeat(80)),
    ] {
        let mut state = selection(
            ModeKeys::Vi,
            &format!("{line}\r\nshort\r\n"),
            &["begin-selection", "end-of-line"],
        );
        let outcome = state
            .execute_command("copy-selection-and-cancel", &[], &context)
            .unwrap();
        assert_eq!(outcome.transfer.unwrap().data, line.as_bytes());
    }
}
