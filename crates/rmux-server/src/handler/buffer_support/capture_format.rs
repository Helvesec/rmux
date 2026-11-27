use rmux_core::{GridRenderOptions, LineFormatCapture};
use rmux_proto::OptionName;

pub(super) fn apply_capture_format_flags(
    content: &mut Vec<u8>,
    request: &rmux_proto::CapturePaneRequest,
    line_format: Option<&LineFormatCapture>,
) {
    if content.is_empty() {
        return;
    }
    // The links of this pane were captured from its own cells upstream, and the
    // primary skips a line that carried no new link BEFORE it writes any number
    // or flag (`cmd-capture-pane.c:255-258`): a capture that found no link has
    // nothing left to prefix and stays empty, writing no buffer at all. The
    // prefixes would in any case carry compacted indexes, not the physical ones
    // the primary numbers with. This early return is the one the base had; the
    // historic limit it carries - `-H` with `-N`/`-F` on a NON-empty capture
    // ignores the prefixes - is left exactly where it was.
    if request.hyperlinks {
        return;
    }
    if !request.line_numbers && !request.include_format {
        return;
    }

    let mut formatted = Vec::new();
    let mut row_numbers = line_format
        .into_iter()
        .flat_map(|format| format.row_numbers().iter().copied());
    let lines = if let Some(physical) =
        line_format.filter(|format| !format.physical_row_ranges().is_empty())
    {
        physical
            .physical_row_ranges()
            .iter()
            .map(|range| &content[range.clone()])
            .collect::<Vec<_>>()
    } else {
        // Pending input has no physical rows and keeps its line-based fallback.
        let mut lines = content.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        if content.ends_with(b"\n") && lines.last().is_some_and(|line| line.is_empty()) {
            let _ = lines.pop();
        }
        lines
    };
    for (index, line) in lines.into_iter().enumerate() {
        let previous_wrapped = index > 0
            && request.join_wrapped
            && line_format
                .and_then(|format| format.join_continuations().get(index - 1))
                .copied()
                .unwrap_or(false);
        if index > 0 && !previous_wrapped {
            formatted.push(b'\n');
        }
        if request.line_numbers {
            // Pending input has no physical grid rows; number its output lines.
            let line_number: i128 = row_numbers.next().unwrap_or(index as i128);
            formatted.extend_from_slice(line_number.to_string().as_bytes());
            formatted.push(b' ');
        }
        if request.include_format {
            formatted.extend_from_slice(
                line_format
                    .and_then(|format| format.labels().get(index))
                    .copied()
                    .unwrap_or("-")
                    .as_bytes(),
            );
            formatted.push(b' ');
        }
        formatted.extend_from_slice(line);
    }
    *content = formatted;
}

pub(super) fn capture_render_options(
    request: &rmux_proto::CapturePaneRequest,
) -> GridRenderOptions {
    let join_wrapped = request.join_wrapped;
    GridRenderOptions {
        // The core retains physical row ranges separately for the formatter;
        // -J must still reach cell rendering to preserve written suffix spaces.
        // Preserve the existing hyperlink/format combination's rendering policy.
        join_wrapped: join_wrapped && !(request.hyperlinks && request.include_format),
        with_sequences: request.escape_ansi || request.hyperlinks,
        escape_sequences: request.escape_sequences,
        include_empty_cells: !join_wrapped && !request.preserve_trailing_spaces,
        use_tmux_cell_capacity: request.do_not_trim_spaces,
        trim_spaces: !join_wrapped && !request.do_not_trim_spaces,
    }
}

pub(super) fn parse_buffer_limit(state: &crate::pane_terminals::HandlerState) -> usize {
    state
        .options
        .resolve(None, OptionName::BufferLimit)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50)
}
