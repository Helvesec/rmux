use rmux_core::GridRenderOptions;
use rmux_proto::OptionName;

pub(super) fn apply_capture_format_flags(
    content: &mut Vec<u8>,
    request: &rmux_proto::CapturePaneRequest,
    line_flags: Option<&[u8]>,
) {
    if request.hyperlinks {
        *content = extract_hyperlink_uris(content);
        return;
    }
    if !request.line_numbers && !request.include_format {
        return;
    }

    let mut formatted = Vec::new();
    let start = request.start.unwrap_or(0);
    let mut lines = content.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if content.ends_with(b"\n") && lines.last().is_some_and(|line| line.is_empty()) {
        let _ = lines.pop();
    }
    for (index, line) in lines.into_iter().enumerate() {
        if index > 0 {
            formatted.push(b'\n');
        }
        if request.line_numbers {
            let line_number = start + i64::try_from(index).unwrap_or(i64::MAX);
            formatted.extend_from_slice(line_number.to_string().as_bytes());
            formatted.push(b' ');
        }
        if request.include_format {
            formatted.push(
                line_flags
                    .and_then(|flags| flags.get(index))
                    .copied()
                    .unwrap_or(b'-'),
            );
            formatted.push(b' ');
        }
        formatted.extend_from_slice(line);
    }
    *content = formatted;
}

fn extract_hyperlink_uris(content: &[u8]) -> Vec<u8> {
    let mut uris = Vec::new();
    let mut index = 0;
    while let Some(start) = find_subslice(&content[index..], b"\x1b]8;") {
        let sequence_start = index + start + b"\x1b]8;".len();
        let Some(params_end) = content[sequence_start..]
            .iter()
            .position(|byte| *byte == b';')
        else {
            break;
        };
        let uri_start = sequence_start + params_end + 1;
        let Some(uri_end) = find_osc_terminator(&content[uri_start..]) else {
            break;
        };
        if uri_end > 0 {
            if !uris.is_empty() {
                uris.push(b'\n');
            }
            uris.extend_from_slice(&content[uri_start..uri_start + uri_end]);
        }
        index = uri_start + uri_end + osc_terminator_len(&content[uri_start + uri_end..]);
    }
    uris
}

fn find_osc_terminator(bytes: &[u8]) -> Option<usize> {
    bytes
        .iter()
        .position(|byte| *byte == 0x07)
        .into_iter()
        .chain(find_subslice(bytes, b"\x1b\\"))
        .min()
}

fn osc_terminator_len(bytes: &[u8]) -> usize {
    if bytes.first().is_some_and(|byte| *byte == 0x07) {
        1
    } else if bytes.starts_with(b"\x1b\\") {
        2
    } else {
        0
    }
}

fn find_subslice(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(super) fn capture_render_options(
    request: &rmux_proto::CapturePaneRequest,
) -> GridRenderOptions {
    let join_wrapped = request.join_wrapped;
    GridRenderOptions {
        join_wrapped,
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
