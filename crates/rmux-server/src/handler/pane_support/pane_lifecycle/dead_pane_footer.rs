//! Remain-on-exit footer and cursor preparation for kept dead panes.

use rmux_proto::{OptionName, PaneTarget, RmuxError, Target};

use super::super::super::scripting_support::format_context_for_target;
use crate::format_runtime::render_runtime_template;
use crate::pane_terminal_lookup::missing_pane_terminal;
use crate::pane_terminals::HandlerState;

pub(super) fn append_remain_on_exit_message(
    state: &mut HandlerState,
    runtime_session_name: &rmux_proto::SessionName,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    let template = state
        .options
        .resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::RemainOnExitFormat,
        )
        .unwrap_or_default();
    if template.is_empty() {
        return Ok(());
    }

    let runtime = format_context_for_target(state, &Target::Pane(target.clone()), 0)?;
    let rendered = render_runtime_template(template, &runtime, false);
    if rendered.is_empty() {
        return Ok(());
    }

    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            missing_pane_terminal(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
        })?;
    // The source draws this status as ONE line of cells of the pane's own
    // width: region `0..sy-1`, a single line feed, the default cell, then
    // `format_draw(&ctx, &gc, sx, expanded, NULL, 0)`
    // (`tmux-3.7b/server-fn.c:359-373`), and that draw copies `width x 1`
    // cells, cutting on whole UTF-8 cells and ignoring controls
    // (`format-draw.c:110-120, 169-194, 789-819`). Writing the expanded text
    // raw let a status longer than the pane wrap onto a second row, which
    // scrolled one more row of the command's own output away.
    let size = state
        .transcript_handle(target)?
        .lock()
        .clone_screen()
        .size();
    let width = usize::from(size.cols);
    let mut bytes = Vec::new();
    if width > 0 {
        let line = crate::renderer::format_draw_line(
            &rendered,
            &rmux_core::style::Style::default(),
            width,
            &rmux_core::Utf8Config::from_options(&state.options),
        );
        // Cells only: a transcript must not be given a cursor save/restore.
        crate::renderer::write_formatted_cells(&mut bytes, &line, 0, line.width());
    }
    state.mutate_runtime_pane_transcript(runtime_session_name, pane_id, true, |transcript| {
        transcript.append_dead_pane_footer(&bytes);
    })
}

/// Hides the cursor of a pane that is kept after its command ended.
///
/// The source does this for every pane it keeps, before and independently of
/// the status it may draw (`tmux-3.7b/server-fn.c:370`), so an empty template
/// still leaves a pane whose cursor is not shown. Only the mode changes: no
/// line feed, no cell written and nothing captured is overwritten.
pub(super) fn hide_dead_pane_cursor(
    state: &mut HandlerState,
    runtime_session_name: &rmux_proto::SessionName,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            missing_pane_terminal(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
        })?;
    state.append_bytes_to_runtime_pane_transcript(runtime_session_name, pane_id, b"\x1b[?25l")?;
    state.hide_runtime_pane_cursor(runtime_session_name, pane_id)
}
