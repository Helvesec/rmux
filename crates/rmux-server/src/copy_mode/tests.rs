use super::types::SelectionMode;
use super::*;
use rmux_core::input::InputParser;
use rmux_proto::TerminalSize;

fn build_screen(cols: u16, rows: u16, content: &str) -> Screen {
    let mut screen = Screen::new(TerminalSize { cols, rows }, 200);
    let mut parser = InputParser::new();
    parser.parse(content.as_bytes(), &mut screen);
    screen
}

fn test_context() -> CopyModeCommandContext {
    CopyModeCommandContext {
        mode_keys: ModeKeys::Emacs,
        wrap_search: true,
        word_separators: " -_@".to_owned(),
        default_shell: "/bin/sh".to_owned(),
        working_directory: None,
        refresh_screen: None,
        mouse: None,
    }
}

#[test]
fn summary_top_line_time_is_zero_for_visible_lines_at_bottom() {
    let screen = build_screen(20, 5, "line1\r\nline2\r\n");
    let state = CopyModeState::for_test(screen);

    assert_eq!(state.summary().top_line_time, 0);
}

#[test]
fn summary_top_line_time_is_preserved_for_history_lines() {
    let screen = build_screen(
        20,
        3,
        "line1\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\n",
    );
    let mut state = CopyModeState::for_test(screen);
    let _ = state.execute_command("history-top", &[], &test_context());

    assert!(
        state.summary().top_line_time > 0,
        "history lines should keep their timestamp for copy-mode-position-format"
    );
}

fn vi_context() -> CopyModeCommandContext {
    CopyModeCommandContext {
        mode_keys: ModeKeys::Vi,
        wrap_search: true,
        word_separators: " -_@".to_owned(),
        default_shell: "/bin/sh".to_owned(),
        working_directory: None,
        refresh_screen: None,
        mouse: None,
    }
}

fn punctuation_word_context() -> CopyModeCommandContext {
    CopyModeCommandContext {
        word_separators: " .-_@".to_owned(),
        ..test_context()
    }
}

#[test]
fn cursor_down_and_cancel_only_cancels_at_bottom() {
    let screen = build_screen(20, 3, "line1\r\nline2\r\nline3");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    // Move to top first.
    let _ = state.execute_command("history-top", &[], &ctx);

    // cursor-down-and-cancel should NOT cancel when not at bottom.
    let outcome = state
        .execute_command("cursor-down-and-cancel", &[], &ctx)
        .unwrap();
    assert!(!outcome.cancel, "should not cancel when cursor moved down");

    // Now go to bottom.
    let _ = state.execute_command("history-bottom", &[], &ctx);

    // cursor-down-and-cancel at the bottom should cancel.
    let outcome = state
        .execute_command("cursor-down-and-cancel", &[], &ctx)
        .unwrap();
    assert!(
        outcome.cancel,
        "should cancel when at bottom and cursor did not move"
    );
}

#[test]
fn scroll_down_and_cancel_only_cancels_at_bottom() {
    let screen = build_screen(20, 3, "line1\r\nline2\r\nline3\r\nline4\r\nline5");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    // Move up to get scroll room.
    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state
        .execute_command("scroll-down-and-cancel", &[], &ctx)
        .unwrap();
    assert!(!outcome.cancel, "should not cancel when not at bottom");

    // Go to bottom.
    let _ = state.execute_command("history-bottom", &[], &ctx);

    let outcome = state
        .execute_command("scroll-down-and-cancel", &[], &ctx)
        .unwrap();
    assert!(outcome.cancel, "should cancel when at bottom");
}

#[test]
fn exit_on_scroll_cancels_scroll_down_at_bottom() {
    let screen = build_screen(20, 3, "line1\r\nline2\r\nline3\r\nline4\r\nline5");
    let mut state = CopyModeState::new(
        screen,
        None,
        false,
        &test_context(),
        true, // exit_on_scroll
        true,
    );
    let ctx = test_context();

    // At the bottom already.
    let outcome = state.execute_command("scroll-down", &[], &ctx).unwrap();
    assert!(
        outcome.cancel,
        "scroll-down should cancel with exit_on_scroll at bottom"
    );
}

#[test]
fn exit_on_scroll_does_not_cancel_when_not_at_bottom() {
    let screen = build_screen(20, 3, "line1\r\nline2\r\nline3\r\nline4\r\nline5");
    let mut state = CopyModeState::new(
        screen,
        None,
        false,
        &test_context(),
        true, // exit_on_scroll
        true,
    );
    let ctx = test_context();

    // Scroll up first.
    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state.execute_command("scroll-down", &[], &ctx).unwrap();
    assert!(
        !outcome.cancel,
        "scroll-down should not cancel when not at bottom"
    );
}

#[test]
fn search_again_advances_to_next_match() {
    let screen = build_screen(20, 3, "foo bar foo baz foo");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    // Initial search.
    let _ = state.execute_command("search-forward", &["--".to_owned(), "foo".to_owned()], &ctx);
    let first = state.cursor;

    // search-again should advance to next match.
    let _ = state.execute_command("search-again", &[], &ctx);
    let second = state.cursor;
    assert!(
        second.x > first.x || second.y > first.y,
        "search-again should advance: first={:?}, second={:?}",
        first,
        second,
    );
}

#[test]
fn oversized_regex_search_marks_partial_without_matches() {
    let screen = build_screen(80, 3, &"a".repeat(1000));
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state
        .execute_command(
            "search-forward",
            &["--".to_owned(), "a{50000}".to_owned()],
            &ctx,
        )
        .unwrap();

    let summary = state.summary();
    assert!(summary.search_count_partial);
    assert_eq!(summary.search_count, 0);
}

#[test]
fn search_again_respects_wrap_search_off() {
    let screen = build_screen(30, 3, "foo bar foo baz foo");
    let mut context = test_context();
    context.wrap_search = false;
    let mut state = CopyModeState::new(screen, None, false, &context, false, true);
    state.cursor = CopyPosition { x: 0, y: 0 };

    let _ = state.execute_command(
        "search-forward",
        &["--".to_owned(), "foo".to_owned()],
        &context,
    );
    let _ = state.execute_command("search-again", &[], &context);
    let _ = state.execute_command("search-again", &[], &context);
    let last = state.cursor;

    let _ = state.execute_command("search-again", &[], &context);

    assert_eq!(state.cursor, last, "search-again should not wrap");
}

#[test]
fn search_reverse_goes_backward_without_changing_direction() {
    let screen = build_screen(30, 3, "foo bar foo baz foo more text");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    // Initial forward search.
    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("search-forward", &["--".to_owned(), "foo".to_owned()], &ctx);
    let _ = state.execute_command("search-again", &[], &ctx);
    let before_reverse = state.cursor;

    // search-reverse should go backward.
    let _ = state.execute_command("search-reverse", &[], &ctx);
    let after_reverse = state.cursor;
    assert!(
        after_reverse.x < before_reverse.x || after_reverse.y < before_reverse.y,
        "search-reverse should go backward: before={:?}, after={:?}",
        before_reverse,
        after_reverse,
    );

    // search-again should still go forward (direction unchanged).
    let _ = state.execute_command("search-again", &[], &ctx);
    let after_again = state.cursor;
    assert!(
        after_again.x > after_reverse.x || after_again.y > after_reverse.y,
        "search-again should still go forward: reverse={:?}, again={:?}",
        after_reverse,
        after_again,
    );
}

#[test]
fn vi_search_positions_at_match_start() {
    let screen = build_screen(30, 3, "hello needle world");
    let mut state = CopyModeState::new(screen, None, false, &vi_context(), false, true);

    let _ = state.execute_command("history-top", &[], &vi_context());
    let _ = state.execute_command(
        "search-forward",
        &["--".to_owned(), "needle".to_owned()],
        &vi_context(),
    );
    assert_eq!(
        state.cursor.x, 6,
        "vi search should position at match start"
    );
}

#[test]
fn jump_to_forward_moves_before_the_matched_character_and_repeats() {
    let screen = build_screen(20, 3, "aXbXcXdXe");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state
        .execute_command("jump-to-forward", &["X".to_owned()], &ctx)
        .unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 2, y: 0 });

    state.execute_command("jump-again", &[], &ctx).unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 4, y: 0 });
}

#[test]
fn jump_to_backward_skips_adjacent_match_and_repeats() {
    let screen = build_screen(20, 3, "aXbXcXdXe");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.execute_command("end-of-line", &[], &ctx).unwrap();

    state
        .execute_command("jump-to-backward", &["X".to_owned()], &ctx)
        .unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 8, y: 0 });

    state.execute_command("jump-again", &[], &ctx).unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 6, y: 0 });

    state.execute_command("jump-again", &[], &ctx).unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 4, y: 0 });

    state.execute_command("jump-again", &[], &ctx).unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 2, y: 0 });
}

#[test]
fn jump_to_backward_skips_adjacent_wide_match() {
    let screen = build_screen(20, 3, "界a界b");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.cursor = CopyPosition { x: 5, y: 0 };
    state
        .execute_command("jump-to-backward", &["界".to_owned()], &ctx)
        .unwrap();

    assert_eq!(state.cursor, CopyPosition { x: 2, y: 0 });
}

#[test]
fn jump_to_forward_skips_adjacent_match_like_tmux() {
    let screen = build_screen(20, 3, "abXd");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state
        .execute_command("jump-to-forward", &["X".to_owned()], &ctx)
        .unwrap();

    assert_eq!(state.cursor, CopyPosition { x: 1, y: 0 });
}

#[test]
fn next_word_stops_on_separator_tokens_like_tmux() {
    let screen = build_screen(30, 5, "foo.bar baz-qux end\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = punctuation_word_context();

    state.execute_command("history-top", &[], &ctx).unwrap();

    for expected in [
        CopyPosition { x: 3, y: 0 },
        CopyPosition { x: 4, y: 0 },
        CopyPosition { x: 8, y: 0 },
        CopyPosition { x: 11, y: 0 },
        CopyPosition { x: 12, y: 0 },
        CopyPosition { x: 16, y: 0 },
        CopyPosition { x: 1, y: 4 },
    ] {
        state.execute_command("next-word", &[], &ctx).unwrap();
        assert_eq!(state.cursor, expected);
    }
}

#[test]
fn next_word_end_stops_after_current_token_like_tmux() {
    let screen = build_screen(30, 5, "foo.bar baz-qux end\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = punctuation_word_context();

    state.execute_command("history-top", &[], &ctx).unwrap();

    for expected in [
        CopyPosition { x: 3, y: 0 },
        CopyPosition { x: 4, y: 0 },
        CopyPosition { x: 7, y: 0 },
        CopyPosition { x: 11, y: 0 },
        CopyPosition { x: 12, y: 0 },
        CopyPosition { x: 15, y: 0 },
        CopyPosition { x: 19, y: 0 },
        CopyPosition { x: 0, y: 4 },
    ] {
        state.execute_command("next-word-end", &[], &ctx).unwrap();
        assert_eq!(state.cursor, expected);
    }
}

#[test]
fn previous_word_stops_on_separator_tokens_like_tmux() {
    let screen = build_screen(30, 5, "foo.bar baz-qux end\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = punctuation_word_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.cursor = CopyPosition { x: 16, y: 0 };

    for expected in [
        CopyPosition { x: 12, y: 0 },
        CopyPosition { x: 11, y: 0 },
        CopyPosition { x: 8, y: 0 },
        CopyPosition { x: 4, y: 0 },
        CopyPosition { x: 3, y: 0 },
        CopyPosition { x: 0, y: 0 },
    ] {
        state.execute_command("previous-word", &[], &ctx).unwrap();
        assert_eq!(state.cursor, expected);
    }
}

#[test]
fn next_space_moves_to_the_terminal_boundary_after_the_last_word_like_tmux() {
    let screen = build_screen(30, 5, "foo.bar baz-qux end\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = punctuation_word_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.cursor = CopyPosition { x: 16, y: 0 };

    state.execute_command("next-space", &[], &ctx).unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 1, y: 4 });
}

#[test]
fn copy_cursor_word_uses_the_next_word_from_separators_like_tmux() {
    let screen = build_screen(30, 5, "foo.bar baz-qux end\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = punctuation_word_context();

    state.execute_command("history-top", &[], &ctx).unwrap();

    state.cursor = CopyPosition { x: 3, y: 0 };
    assert_eq!(state.summary().copy_cursor_word, "bar");

    state.cursor = CopyPosition { x: 11, y: 0 };
    assert_eq!(state.summary().copy_cursor_word, "qux");

    state.cursor = CopyPosition { x: 1, y: 4 };
    assert_eq!(state.summary().copy_cursor_word, "");
}

#[test]
fn matching_brackets_match_naive_ascii_oracle() {
    let text = "a(b[c{d}e]f) z";
    let screen = build_screen(30, 3, text);
    let mut state = CopyModeState::for_test(screen);

    state
        .execute_command("history-top", &[], &test_context())
        .unwrap();
    for cursor_x in [1, 3, 5, 7, 9, 11] {
        state.cursor = CopyPosition { x: cursor_x, y: 0 };
        for forward in [true, false] {
            let expected =
                naive_matching_bracket(text, cursor_x, forward).map(|x| CopyPosition { x, y: 0 });
            assert_eq!(
                state.find_matching_bracket(forward),
                expected,
                "cursor_x={cursor_x}, forward={forward}"
            );
        }
    }
}

#[test]
fn matching_brackets_scan_across_lines_without_flattening_buffer() {
    let screen = build_screen(10, 6, "(\r\n[\r\n]\r\n)");
    let mut state = CopyModeState::for_test(screen);
    state
        .execute_command("history-top", &[], &test_context())
        .unwrap();

    state.cursor = CopyPosition { x: 0, y: 0 };
    assert_eq!(
        state.find_matching_bracket(true),
        Some(CopyPosition { x: 0, y: 3 })
    );

    state.cursor = CopyPosition { x: 0, y: 3 };
    assert_eq!(
        state.find_matching_bracket(true),
        Some(CopyPosition { x: 0, y: 0 })
    );
}

fn naive_matching_bracket(text: &str, cursor_x: u32, forward: bool) -> Option<u32> {
    let chars = text.chars().collect::<Vec<_>>();
    let current = *chars.get(cursor_x as usize)?;
    let (open, close, scan_forward) = match current {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _ => return None,
    };
    let scan_forward = if forward { scan_forward } else { !scan_forward };
    let mut depth = 1usize;
    if scan_forward {
        for (index, ch) in chars
            .iter()
            .copied()
            .enumerate()
            .skip(cursor_x as usize + 1)
        {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index as u32);
                }
            }
        }
    } else {
        for (index, ch) in chars
            .iter()
            .copied()
            .enumerate()
            .take(cursor_x as usize)
            .rev()
        {
            if ch == close {
                depth += 1;
            } else if ch == open {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index as u32);
                }
            }
        }
    }
    None
}

#[test]
fn emacs_search_positions_past_match_end() {
    let screen = build_screen(30, 3, "hello needle world");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command(
        "search-forward",
        &["--".to_owned(), "needle".to_owned()],
        &ctx,
    );
    // "needle" starts at col 6, ends at col 11.
    assert_eq!(
        state.cursor.x, 11,
        "emacs search should position at match end"
    );
}

#[test]
fn view_mode_blocks_non_readonly_commands() {
    let screen = build_screen(20, 3, "hello world");
    let mut state = CopyModeState::new(
        screen,
        None,
        true, // view_mode
        &test_context(),
        false,
        true,
    );
    let ctx = test_context();

    // Readonly commands should work.
    let outcome = state.execute_command("cursor-down", &[], &ctx).unwrap();
    assert!(!outcome.cancel);

    // Non-readonly commands should be silently ignored.
    let outcome = state.execute_command("begin-selection", &[], &ctx).unwrap();
    assert!(!outcome.cancel);
    assert!(
        state.selection.is_none(),
        "view-mode should block begin-selection"
    );
}

#[test]
fn copy_selection_with_no_selection_yields_empty_data() {
    let screen = build_screen(20, 3, "hello");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let outcome = state
        .execute_command("copy-selection-and-cancel", &[], &ctx)
        .unwrap();
    assert!(outcome.cancel);
    let transfer = outcome.transfer.unwrap();
    assert!(
        transfer.data.is_empty(),
        "should produce empty data when no selection"
    );
}

#[test]
fn character_selection_excludes_the_cursor_cell_like_tmux() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\ngamma\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("cursor-down", &[], &ctx);
    let _ = state.execute_command("begin-selection", &[], &ctx);
    let _ = state.execute_command("cursor-right", &[], &ctx);

    let outcome = state
        .execute_command("copy-selection-and-cancel", &[], &ctx)
        .unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"b");
}

#[test]
fn cursor_right_wraps_after_logical_line_end() {
    let screen = build_screen(30, 5, "foo.bar baz-qux end\r\n\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    for _ in 0..20 {
        let _ = state.execute_command("cursor-right", &[], &ctx);
    }

    let summary = state.summary();
    assert_eq!(summary.cursor_x, 0);
    assert_eq!(summary.cursor_y, 1);
    assert_eq!(summary.copy_cursor_line, "");
}

#[test]
fn multiline_character_selection_excludes_first_cell_of_end_line_like_tmux() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\ngamma\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("cursor-down", &[], &ctx);
    let _ = state.execute_command("begin-selection", &[], &ctx);
    let _ = state.execute_command("cursor-down", &[], &ctx);

    let outcome = state
        .execute_command("copy-selection-and-cancel", &[], &ctx)
        .unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"beta\n");
}

#[test]
fn middle_line_uses_upper_middle_on_even_height_like_tmux() {
    let screen = build_screen(
        20,
        6,
        "line1\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\n",
    );
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("middle-line", &[], &ctx);

    assert_eq!(state.summary().cursor_y, 2);
    assert_eq!(state.summary().copy_cursor_line, "line3");
}

#[test]
fn goto_line_scrolls_from_bottom_like_tmux() {
    let screen = build_screen(
        30,
        4,
        "L1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7\r\nL8\r\nL9\r\nL10\r\nPROMPT",
    );
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.set_show_position(false);
    let _ = state.execute_command("goto-line", &["1".to_owned()], &ctx);
    let summary = state.summary();
    assert_eq!(summary.scroll_position, 1);
    assert_eq!(summary.cursor_y, 3);
    assert_eq!(summary.copy_cursor_line, "L10");

    let _ = state.execute_command("goto-line", &["999".to_owned()], &ctx);
    let summary = state.summary();
    assert_eq!(summary.scroll_position, 7);
    assert_eq!(summary.cursor_y, 3);
    assert_eq!(summary.copy_cursor_line, "L4");

    let _ = state.execute_command("goto-line", &["bad".to_owned()], &ctx);
    let summary = state.summary();
    assert_eq!(summary.scroll_position, 7);
    assert_eq!(summary.copy_cursor_line, "L4");

    let _ = state.execute_command("goto-line", &["-5".to_owned()], &ctx);
    let summary = state.summary();
    assert_eq!(summary.scroll_position, 7);
    assert_eq!(summary.copy_cursor_line, "L4");
}

#[test]
fn line_selection_omits_trailing_newline() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\ngamma\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("select-line", &[], &ctx);
    assert_eq!(state.summary().cursor_x, 5);
    assert_eq!(
        state.summary().selection_end.unwrap(),
        CopyPosition { x: 5, y: 0 }
    );

    let outcome = state.execute_command("copy-selection", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"alpha");
}

#[test]
fn vi_line_selection_includes_trailing_newline() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\ngamma\r\n");
    let mut state = CopyModeState::new(screen, None, false, &vi_context(), false, true);
    let ctx = vi_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("select-line", &[], &ctx);

    let outcome = state.execute_command("copy-selection", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"alpha\n");
}

#[test]
fn line_selection_keeps_internal_newlines_without_trailing_newline() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\ngamma\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("select-line", &[], &ctx);
    let _ = state.execute_command("cursor-down", &[], &ctx);

    let outcome = state.execute_command("copy-selection", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"alpha\nbeta");
}

#[test]
fn line_selection_joins_wrapped_physical_rows_without_trailing_newline() {
    let screen = build_screen(20, 4, "ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("select-line", &[], &ctx);

    let outcome = state.execute_command("copy-selection", &[], &ctx).unwrap();
    assert_eq!(
        outcome.transfer.unwrap().data,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZ"
    );
}

#[test]
fn character_selection_joins_wrapped_physical_rows_without_newlines() {
    let screen = build_screen(10, 8, "0123456789ABCDEFGHIJKLMNO\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.execute_command("start-of-line", &[], &ctx).unwrap();
    state.execute_command("begin-selection", &[], &ctx).unwrap();
    state.execute_command("end-of-line", &[], &ctx).unwrap();

    let outcome = state.execute_command("copy-selection", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"0123456789ABCDEFGHIJKLMNO");
}

#[test]
fn copy_pipe_uses_wrapped_character_selection_without_newlines() {
    let screen = build_screen(10, 8, "0123456789ABCDEFGHIJKLMNO\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.execute_command("start-of-line", &[], &ctx).unwrap();
    state.execute_command("begin-selection", &[], &ctx).unwrap();
    state.execute_command("end-of-line", &[], &ctx).unwrap();

    let outcome = state.execute_command("copy-pipe", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"0123456789ABCDEFGHIJKLMNO");
}

#[test]
fn copy_line_omits_trailing_newline() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state.execute_command("copy-line", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"alpha");
}

#[test]
fn vi_copy_line_includes_trailing_newline() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\n");
    let mut state = CopyModeState::new(screen, None, false, &vi_context(), false, true);
    let ctx = vi_context();

    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state.execute_command("copy-line", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"alpha\n");
}

#[test]
fn copy_line_on_empty_line_yields_empty_data() {
    let screen = build_screen(20, 3, "\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state.execute_command("copy-line", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"");
}

#[test]
fn vi_copy_line_on_empty_line_yields_newline() {
    let screen = build_screen(20, 3, "\r\n");
    let mut state = CopyModeState::new(screen, None, false, &vi_context(), false, true);
    let ctx = vi_context();

    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state.execute_command("copy-line", &[], &ctx).unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"\n");
}

#[test]
fn copy_end_of_line_omits_trailing_newline() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state
        .execute_command("copy-end-of-line", &[], &ctx)
        .unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"alpha");
}

#[test]
fn vi_copy_end_of_line_includes_trailing_newline() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\n");
    let mut state = CopyModeState::new(screen, None, false, &vi_context(), false, true);
    let ctx = vi_context();

    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state
        .execute_command("copy-end-of-line", &[], &ctx)
        .unwrap();
    assert_eq!(outcome.transfer.unwrap().data, b"alpha\n");
}

#[test]
fn end_of_line_moves_to_wrapped_logical_line_end_like_tmux() {
    let screen = build_screen(20, 4, "ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("end-of-line", &[], &ctx);

    assert_eq!(state.summary().cursor_x, 6);
    assert_eq!(state.summary().cursor_y, 1);
}

#[test]
fn copy_end_of_line_uses_wrapped_logical_line_without_trailing_newline() {
    let screen = build_screen(20, 4, "ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);

    let outcome = state
        .execute_command("copy-end-of-line", &[], &ctx)
        .unwrap();
    assert_eq!(
        outcome.transfer.unwrap().data,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZ"
    );
}

#[test]
fn end_of_line_stops_after_content_like_tmux() {
    let screen = build_screen(20, 3, "alpha\r\nbeta\r\ngamma\r\n");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("end-of-line", &[], &ctx);

    assert_eq!(state.summary().cursor_x, 5);
}

#[test]
fn clear_policy_emacs_only_clears_in_emacs_mode() {
    let screen = build_screen(30, 3, "hello world needle");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command(
        "search-forward",
        &["--".to_owned(), "needle".to_owned()],
        &ctx,
    );
    assert!(state.search_highlighted);

    // cursor-down has EmacsOnly clear policy; in emacs mode it should clear.
    let _ = state.execute_command("cursor-down", &[], &ctx);
    assert!(
        !state.search_highlighted,
        "emacs mode cursor-down should clear highlights"
    );
}

#[test]
fn clear_policy_emacs_only_does_not_clear_in_vi_mode() {
    let screen = build_screen(30, 3, "hello world needle");
    let mut state = CopyModeState::new(screen, None, false, &vi_context(), false, true);
    let ctx = vi_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command(
        "search-forward",
        &["--".to_owned(), "needle".to_owned()],
        &ctx,
    );
    assert!(state.search_highlighted);

    // cursor-down has EmacsOnly clear policy; in vi mode it should NOT clear.
    let _ = state.execute_command("cursor-down", &[], &ctx);
    assert!(
        state.search_highlighted,
        "vi mode cursor-down should not clear highlights"
    );
}

#[test]
fn selection_mode_switches_existing_selection() {
    let screen = build_screen(30, 3, "hello world");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("begin-selection", &[], &ctx);
    assert_eq!(state.selection.as_ref().unwrap().mode, SelectionMode::Char);

    let _ = state.execute_command("selection-mode", &["word".to_owned()], &ctx);
    assert_eq!(state.selection.as_ref().unwrap().mode, SelectionMode::Word);
}

#[test]
fn mark_and_jump_to_mark() {
    let screen = build_screen(20, 5, "line1\r\nline2\r\nline3\r\nline4\r\nline5");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let _ = state.execute_command("history-top", &[], &ctx);
    let _ = state.execute_command("set-mark", &[], &ctx);
    let mark_pos = state.cursor;

    let _ = state.execute_command("cursor-down", &[], &ctx);
    let _ = state.execute_command("cursor-down", &[], &ctx);
    assert_ne!(state.cursor, mark_pos);

    let _ = state.execute_command("jump-to-mark", &[], &ctx);
    assert_eq!(state.cursor, mark_pos, "should jump back to mark position");
}

#[test]
fn unknown_command_returns_error() {
    let screen = build_screen(20, 3, "hello");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    let result = state.execute_command("not-a-real-command", &[], &ctx);
    assert!(result.is_err());
}

#[test]
fn rg15_next_word_at_end_lands_after_last_word_when_last_row_is_populated() {
    let screen = build_screen(30, 3, "alpha\r\nbeta\r\ngamma");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    assert_eq!(state.cursor, CopyPosition { x: 0, y: 0 });

    for _ in 0..10 {
        state.execute_command("next-word", &[], &ctx).unwrap();
    }
    assert_eq!(state.cursor, CopyPosition { x: 5, y: 2 });
    assert_eq!(state.summary().copy_cursor_word, "");
}

#[test]
fn rg15_next_word_single_line_lands_after_only_word() {
    let screen = build_screen(30, 1, "alpharbeta");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.cursor = CopyPosition { x: 0, y: 0 };

    state.execute_command("next-word", &[], &ctx).unwrap();
    let after_first = state.cursor;
    state.execute_command("next-word", &[], &ctx).unwrap();
    let after_second = state.cursor;

    assert_eq!(after_first, CopyPosition { x: 10, y: 0 });
    assert_eq!(after_second, CopyPosition { x: 10, y: 0 });
    assert_eq!(state.summary().copy_cursor_word, "");
}

#[test]
fn next_word_at_full_width_final_line_stays_at_logical_end() {
    let screen = build_screen(5, 1, "abcde");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    state.execute_command("next-word", &[], &ctx).unwrap();
    state.execute_command("next-word", &[], &ctx).unwrap();

    assert_eq!(state.cursor, CopyPosition { x: 5, y: 0 });
    assert_eq!(state.summary().copy_cursor_word, "");
}

#[test]
fn rg15_next_space_at_end_lands_after_last_word_when_last_row_is_populated() {
    let screen = build_screen(30, 3, "alpha\r\nbeta\r\ngamma");
    let mut state = CopyModeState::for_test(screen);
    let ctx = test_context();

    state.execute_command("history-top", &[], &ctx).unwrap();
    for _ in 0..10 {
        state.execute_command("next-space", &[], &ctx).unwrap();
    }
    assert_eq!(state.cursor, CopyPosition { x: 5, y: 2 });
    assert_eq!(state.summary().copy_cursor_word, "");
}
