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
