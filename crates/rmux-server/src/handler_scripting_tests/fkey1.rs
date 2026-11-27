//! FKEY1 — a finished command kept in a float until a key closes it.
//!
//! The expectations are what the pinned tmux 3.7b did before any line of this
//! lot (`fkey1/native/oracle-fkey1.json`): `new-pane -k` writes the
//! pane's own `remain-on-exit` as `key`, and that local value is what decides,
//! whatever the global option says.

use super::*;
use rmux_proto::OptionName;

/// The value written on the pane itself, never one resolved from a wider scope.
async fn local_policy(handler: &RequestHandler, pane_index: u32) -> Option<String> {
    handler
        .state
        .lock()
        .await
        .options
        .pane_value(
            &rmux_proto::PaneTarget::with_window(session_name("fnp"), 0, pane_index),
            OptionName::RemainOnExit,
        )
        .map(str::to_owned)
}

/// What the pane answers with, wider scopes included.
async fn resolved_policy(handler: &RequestHandler, pane_index: u32) -> Option<String> {
    handler
        .state
        .lock()
        .await
        .options
        .resolve_for_pane(
            &session_name("fnp"),
            0,
            pane_index,
            OptionName::RemainOnExit,
        )
        .map(str::to_owned)
}

/// Opens a float and returns the pane index it was given.
async fn opened(handler: &RequestHandler, args: &[&str]) -> u32 {
    let mut command = vec!["newp", "-dP", "-F", "#{pane_index}"];
    command.extend_from_slice(args);
    match offer(handler, &command, true).await {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            exit_status: 0,
            ..
        } => String::from_utf8(stdout)
            .expect("printed index is utf-8")
            .trim_end()
            .parse()
            .expect("the printed index is a number"),
        other => panic!("{args:?}: {other:?}"),
    }
}

#[tokio::test]
async fn the_flag_writes_the_panes_own_policy_through_both_entries() {
    let handler = setup().await;
    // Measured (cell k01): the source sets `remain-on-exit` on the new pane
    // itself, and reads it as `key`.
    let cli = opened(&handler, &["-k"]).await;
    assert_eq!(local_policy(&handler, cli).await.as_deref(), Some("key"));
    // The queue is the other entry of the same admission, and a sourced file
    // reaches it the same way.
    queue(&handler, "new-pane -d -k -t fnp:0.0")
        .await
        .expect("the queue opens the same float");
    let queued = window(&handler).await.pane_count() - 1;
    assert_eq!(
        local_policy(&handler, u32::try_from(queued).unwrap())
            .await
            .as_deref(),
        Some("key")
    );
    // Without the flag nothing is written on the pane at all, so what it
    // answers comes from the wider scope.
    let plain = opened(&handler, &[]).await;
    assert_eq!(local_policy(&handler, plain).await, None);
    assert_eq!(
        resolved_policy(&handler, plain).await.as_deref(),
        Some("off")
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_panes_own_policy_decides_against_any_global_value() {
    let handler = setup().await;
    // Measured (cell k03): with the global option `off`, a float opened with
    // `-k` still holds `key`; `failed` is the other value that would not have
    // kept a command which ended with status zero.
    for global in ["off", "failed", "on"] {
        queue(&handler, &format!("set-option -g remain-on-exit {global}"))
            .await
            .expect("the global value is written");
        let pane = opened(&handler, &["-k"]).await;
        assert_eq!(
            local_policy(&handler, pane).await.as_deref(),
            Some("key"),
            "global {global}"
        );
        assert_eq!(
            resolved_policy(&handler, pane).await.as_deref(),
            Some("key"),
            "global {global}: the pane's own value is what it answers with"
        );
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_refused_opening_writes_no_policy_and_creates_nothing() {
    let handler = setup().await;
    let before = window(&handler).await.pane_count();
    // The rectangle is refused before anything is created, and the pane-local
    // option the flag would have written is never left behind.
    let error = queue(&handler, "new-pane -d -k -X 81 -t fnp:0.0")
        .await
        .expect_err("a position past the edge is refused");
    assert!(error.to_string().contains("out of bounds"), "{error}");
    assert_eq!(window(&handler).await.pane_count(), before);
    assert_eq!(
        local_policy(&handler, u32::try_from(before).unwrap()).await,
        None,
        "no policy was written for a pane that was never created"
    );
    cleanup(&handler).await;
}

/// What the float holds once its command has ended, line by line.
async fn dead_capture(handler: &RequestHandler, pane_index: u32) -> Vec<String> {
    let output = queue(handler, &format!("capture-pane -p -t fnp:0.{pane_index}"))
        .await
        .expect("the float is captured");
    String::from_utf8(output.stdout().to_vec())
        .expect("a capture is utf-8")
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// The identifier of the float that was just created at this index.
async fn float_pane_id(handler: &RequestHandler, pane_index: u32) -> rmux_core::PaneId {
    window(handler)
        .await
        .panes()
        .iter()
        .find(|pane| pane.index() == pane_index)
        .map(rmux_core::Pane::id)
        .expect("the float that was just created is in the window")
}

/// Waits, bounded, for the end these witnesses are about: the float's own
/// `PaneDied`, correlated to its pane.
///
/// `#{pane_dead}` is not that end. It answers from the exit metadata
/// (`format_runtime.rs:422`), which `pane_outputs.rs:160-166` writes as soon as
/// the child's status is known, before the kept pane is committed. The status
/// this lot draws is appended after that, and the cursor is hidden after the
/// append (`handler_pane/lifecycle.rs:785-792`, `:809`, `:824`); `PaneDied` is
/// prepared at `:837` and published once those have returned, so it is the
/// first signal that says the screen is finished. It is the server's own
/// signal, independent of every value these witnesses go on to read.
async fn wait_for_float_death(
    events: &mut tokio::sync::broadcast::Receiver<crate::handler::QueuedLifecycleEvent>,
    pane_index: u32,
    pane_id: rmux_core::PaneId,
) {
    let target = rmux_proto::PaneTarget::with_window(session_name("fnp"), 0, pane_index);
    let deadline = tokio::time::Instant::now() + SERVER_SIGNAL_LIVENESS;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the float at {target:?} never published its death"
        );
        match tokio::time::timeout(remaining, events.recv()).await {
            Err(_) => panic!("the float at {target:?} never published its death"),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(missed))) => {
                panic!("{missed} lifecycle events were lost before the death of {target:?}")
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                panic!("the lifecycle channel closed before the death of {target:?}")
            }
            // Everything addressed to another pane is passed over; nothing else
            // is taken for this end.
            Ok(Ok(event)) => {
                if let rmux_core::LifecycleEvent::PaneDied {
                    target: died,
                    pane_id: died_id,
                    ..
                } = event.event_for_test()
                {
                    if *died == target && *died_id == Some(pane_id.as_u32()) {
                        return;
                    }
                }
            }
        }
    }
}

/// Opens a float that writes its own lines and ends, and waits for its death.
///
/// The command is built by this suite's own platform boundary, so the same
/// content is written and the same zero status returned on every platform: a
/// hard-coded `/bin/sh -c 'printf …'` would simply not run on Windows.
///
/// That boundary builds the fragment for the interpreter the suite runs its
/// commands with, so the same boundary has to select it first: without this
/// precondition a Windows run hands a PowerShell fragment to whatever shell the
/// product resolved, and the float ends on that shell's own failure instead of
/// the zero status the witnesses read. On Unix the call is a no-op.
async fn finished(handler: &RequestHandler, args: &[&str], lines: &[&str]) -> u32 {
    use_platform_test_shell(handler).await;
    let written = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let script = shell_print_then_exit_command(&written, 0);
    let mut command = vec!["newp", "-dP", "-F", "#{pane_index}", "-k"];
    command.extend_from_slice(args);
    command.push(&script);
    // Subscribed before the float exists: a death already queued when the
    // creation returns is still readable here, and a receiver opened afterwards
    // would have missed it.
    let mut events = handler.subscribe_lifecycle_events();
    let pane = match offer(handler, &command, true).await {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            exit_status: 0,
            ..
        } => String::from_utf8(stdout)
            .expect("printed index is utf-8")
            .trim_end()
            .parse::<u32>()
            .expect("the printed index is a number"),
        other => panic!("{args:?}: {other:?}"),
    };
    let pane_id = float_pane_id(handler, pane).await;
    wait_for_float_death(&mut events, pane, pane_id).await;
    // The precondition of this fragment, on the pane the death named: it wrote
    // its lines and left with status zero, so what follows reads a status line
    // drawn for a command that ended by itself.
    let metadata = handler
        .state
        .lock()
        .await
        .pane_exit_metadata(&session_name("fnp"), pane_id)
        .expect("the dead float carries its own exit metadata");
    assert_eq!(metadata.status, Some(0), "{args:?}");
    assert_eq!(metadata.signal, None, "{args:?}");
    pane
}

#[tokio::test]
async fn a_status_longer_than_the_float_takes_one_line_and_keeps_the_output() {
    let handler = setup().await;
    // The fixture window is 80x24, so the default float is 40x6 and the status
    // this server renders is longer than that. Measured on the pin (cell k01):
    // the status is drawn as one line of the pane's own width, so the line feed
    // that precedes it scrolls exactly one row away and the two last lines the
    // command wrote are still there.
    let pane = finished(&handler, &[], &["FKEY1-1", "FKEY1-2", "FKEY1-3"]).await;
    let capture = dead_capture(&handler, pane).await;
    assert_eq!(
        capture.len(),
        7,
        "six rows and the trailing empty split: {capture:?}"
    );
    assert_eq!(&capture[..2], ["FKEY1-2", "FKEY1-3"], "{capture:?}");
    let status: Vec<&String> = capture
        .iter()
        .filter(|line| line.contains("Pane is dead"))
        .collect();
    assert_eq!(status.len(), 1, "one status line only: {capture:?}");
    assert!(
        status[0].chars().count() <= 40,
        "the status is bounded by the pane's width: {:?}",
        status[0]
    );
    // What follows the status line is empty: nothing wrapped onto it.
    let index = capture
        .iter()
        .position(|line| line.contains("Pane is dead"))
        .expect("the status line is there");
    assert!(
        capture[index + 1..].iter().all(|line| line.is_empty()),
        "{capture:?}"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_status_wider_than_a_cell_is_cut_on_whole_cells_and_keeps_its_style() {
    let handler = setup().await;
    // The drawing cuts on whole UTF-8 cells and reads `#[...]` clauses, so a
    // template whose text is wide characters ends on a cell boundary and no
    // clause is ever written out as literal text.
    queue(
        &handler,
        "set-option -g remain-on-exit-format '#[bold]\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}\u{1f600}'",
    )
    .await
    .expect("the template is written");
    let pane = finished(&handler, &[], &["WIDE-1", "WIDE-2", "WIDE-3"]).await;
    let capture = dead_capture(&handler, pane).await;
    assert_eq!(&capture[..2], ["WIDE-2", "WIDE-3"], "{capture:?}");
    let status = capture
        .iter()
        .find(|line| line.contains('\u{1f600}'))
        .expect("the drawn status is there");
    assert!(
        !status.contains("#["),
        "no clause is written literally: {status:?}"
    );
    assert_eq!(
        status.chars().filter(|c| *c == '\u{1f600}').count(),
        20,
        "whole cells only, twenty of them in forty columns: {status:?}"
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn a_short_status_and_an_empty_one_are_left_as_they_were() {
    let handler = setup().await;
    queue(&handler, "set-option -g remain-on-exit-format 'gone'")
        .await
        .expect("the template is written");
    let short = finished(&handler, &[], &["SHORT-1", "SHORT-2", "SHORT-3"]).await;
    let capture = dead_capture(&handler, short).await;
    assert_eq!(&capture[..2], ["SHORT-2", "SHORT-3"], "{capture:?}");
    assert!(capture.iter().any(|line| line == "gone"), "{capture:?}");
    // An empty template writes nothing at all, not even a line feed, so the
    // command's own last line stays where it was.
    queue(&handler, "set-option -g remain-on-exit-format ''")
        .await
        .expect("the template is emptied");
    let empty = finished(&handler, &[], &["EMPTY-1", "EMPTY-2", "EMPTY-3"]).await;
    let capture = dead_capture(&handler, empty).await;
    assert_eq!(
        &capture[..3],
        ["EMPTY-1", "EMPTY-2", "EMPTY-3"],
        "{capture:?}"
    );
    assert!(
        capture.iter().all(|line| !line.contains("Pane is dead")),
        "{capture:?}"
    );
    cleanup(&handler).await;
}

/// The float's own screen, once its command has ended.
async fn dead_screen(handler: &RequestHandler, pane_index: u32) -> rmux_core::Screen {
    let target = rmux_proto::PaneTarget::with_window(session_name("fnp"), 0, pane_index);
    let state = handler.state.lock().await;
    let transcript = state.transcript_handle(&target).expect("the float has one");
    let screen = transcript.lock().clone_screen();
    screen
}

/// The modes of that same screen.
async fn dead_screen_mode(handler: &RequestHandler, pane_index: u32) -> u32 {
    let target = rmux_proto::PaneTarget::with_window(session_name("fnp"), 0, pane_index);
    let state = handler.state.lock().await;
    let transcript = state.transcript_handle(&target).expect("the float has one");
    let mode = transcript.lock().mode();
    mode
}

#[tokio::test]
async fn the_status_lands_on_the_last_row_whatever_margins_the_command_left() {
    // The source sets the region to the whole pane before its single line feed
    // (`tmux-3.7b/server-fn.c:359-373`). A command is free to leave a smaller
    // one, and to leave origin mode on with it: the status must still be the
    // last row and the two last lines written must still be there.
    for origin in [false, true] {
        let handler = setup().await;
        // A real escape byte, so what the pane receives does not depend on one
        // shell interpreting backslashes.
        let leave = if origin {
            "\u{1b}[2;4r\u{1b}[?6h"
        } else {
            "\u{1b}[2;4r"
        };
        let pane = finished(&handler, &[], &[leave, "MARGIN-1", "MARGIN-2", "MARGIN-3"]).await;
        let capture = dead_capture(&handler, pane).await;
        let status: Vec<usize> = capture
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains("Pane is dead"))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(status.len(), 1, "origin={origin}: {capture:?}");
        assert_eq!(
            status[0], 5,
            "origin={origin}: the status is the last row of the float: {capture:?}"
        );
        assert_eq!(
            &capture[..2],
            ["MARGIN-2", "MARGIN-3"],
            "origin={origin}: {capture:?}"
        );
        cleanup(&handler).await;
    }
}

#[tokio::test]
async fn the_cursor_of_a_kept_pane_is_hidden_even_with_no_status_to_draw() {
    let handler = setup().await;
    // The source hides it for every pane it keeps, before and independently of
    // the format (`server-fn.c:370`), so an empty template still leaves a pane
    // whose cursor is not shown - and writes no line feed.
    queue(&handler, "set-option -g remain-on-exit-format ''")
        .await
        .expect("the template is emptied");
    let pane = finished(&handler, &[], &["HIDDEN-1", "HIDDEN-2", "HIDDEN-3"]).await;
    let mode = dead_screen_mode(&handler, pane).await;
    assert_eq!(
        mode & rmux_core::input::mode::MODE_CURSOR,
        0,
        "the kept pane's cursor is hidden"
    );
    let capture = dead_capture(&handler, pane).await;
    assert_eq!(
        &capture[..3],
        ["HIDDEN-1", "HIDDEN-2", "HIDDEN-3"],
        "nothing was written and nothing scrolled: {capture:?}"
    );
    // And with a status to draw, it is hidden too.
    queue(&handler, "set-option -g remain-on-exit-format 'gone'")
        .await
        .expect("the template is written");
    let drawn = finished(&handler, &[], &["SHOWN-1", "SHOWN-2", "SHOWN-3"]).await;
    assert_eq!(
        dead_screen_mode(&handler, drawn).await & rmux_core::input::mode::MODE_CURSOR,
        0
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_status_starts_from_the_default_pen_whatever_the_command_left() {
    let handler = setup().await;
    // A command that leaves bold on must not colour this status bold: the
    // source starts its draw from the default cell, so the pen is reset before
    // the line is written and `#[fg=red]` is red and nothing else.
    queue(
        &handler,
        "set-option -g remain-on-exit-format '#[fg=red]DEAD'",
    )
    .await
    .expect("the template is written");
    let pane = finished(&handler, &[], &["\u{1b}[1m", "BOLD-1", "BOLD-2", "BOLD-3"]).await;
    let screen = dead_screen(&handler, pane).await;
    let mut cells = Vec::new();
    assert!(screen.visit_visible_line_cells(5, 4, |cell| cells.push((cell.attr(), cell.fg()))));
    assert!(!cells.is_empty(), "the status row has cells");
    for (attr, fg) in &cells {
        assert_eq!(
            attr & rmux_core::GridAttr::BRIGHT,
            0,
            "the status kept no attribute of the command: {cells:?}"
        );
        assert_eq!(
            *fg, 1,
            "the status is the red its clause asked for: {cells:?}"
        );
    }
    cleanup(&handler).await;
}

#[tokio::test]
async fn the_status_reads_the_cursor_the_command_left_and_the_pane_ends_hidden() {
    let handler = setup().await;
    // The source expands the format and hides the cursor after it
    // (`tmux-3.7b/server-fn.c:364` then `370`), so a template that asks for
    // `#{cursor_flag}` is answered with the cursor the command left - visible
    // here - while the pane still ends with its cursor hidden.
    queue(
        &handler,
        "set-option -g remain-on-exit-format 'cursor=#{cursor_flag}'",
    )
    .await
    .expect("the template is written");
    let visible = finished(&handler, &[], &["CURSOR-1", "CURSOR-2", "CURSOR-3"]).await;
    let capture = dead_capture(&handler, visible).await;
    assert_eq!(
        capture[5], "cursor=1",
        "the status was expanded before the cursor was hidden: {capture:?}"
    );
    assert_eq!(
        dead_screen_mode(&handler, visible).await & rmux_core::input::mode::MODE_CURSOR,
        0,
        "and the pane still ends with its cursor hidden"
    );

    // The same order before the early return an empty expansion takes: this
    // template renders nothing precisely because the cursor was still visible,
    // so it writes no line feed, overwrites no capture, and the pane ends
    // hidden all the same.
    queue(
        &handler,
        "set-option -g remain-on-exit-format '#{?cursor_flag,,wrong}'",
    )
    .await
    .expect("the conditional template is written");
    let conditional = finished(&handler, &[], &["EMPTY-1", "EMPTY-2", "EMPTY-3"]).await;
    let capture = dead_capture(&handler, conditional).await;
    assert_eq!(
        &capture[..3],
        ["EMPTY-1", "EMPTY-2", "EMPTY-3"],
        "nothing was written and nothing scrolled: {capture:?}"
    );
    assert!(
        capture.iter().all(|line| line != "wrong"),
        "the expansion saw the cursor the command left: {capture:?}"
    );
    assert_eq!(
        dead_screen_mode(&handler, conditional).await & rmux_core::input::mode::MODE_CURSOR,
        0,
        "the pane ends hidden even when nothing was drawn"
    );
    cleanup(&handler).await;
}
