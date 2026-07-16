use std::path::Path;

use rmux_core::{
    formats::{is_truthy, FormatContext},
    BufferView,
};
use rmux_proto::CommandOutput;

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

const DEFAULT_LIST_BUFFERS_TEMPLATE: &str =
    "#{buffer_name}: #{buffer_size} bytes: \"#{buffer_sample}\"";

pub(super) fn render_list_buffer_line(
    state: &crate::pane_terminals::HandlerState,
    socket_path: &Path,
    request: &rmux_proto::ListBuffersRequest,
    entry: BufferView<'_>,
) -> Option<String> {
    let context = RuntimeFormatContext::new(FormatContext::new())
        .with_state(state)
        .with_socket_path(socket_path)
        .with_named_value("buffer_name", entry.name())
        .with_named_value("buffer_size", entry.size().to_string())
        .with_named_value("buffer_sample", entry.sample())
        .with_named_value("buffer_created", entry.created().to_string());

    if let Some(filter) = request.filter.as_deref() {
        let rendered = render_runtime_template(filter, &context, false);
        if !is_truthy(&rendered) {
            return None;
        }
    }

    Some(render_runtime_template(
        request
            .format
            .as_deref()
            .unwrap_or(DEFAULT_LIST_BUFFERS_TEMPLATE),
        &context,
        false,
    ))
}

pub(super) fn command_output_from_lines(lines: &[String]) -> CommandOutput {
    if lines.is_empty() {
        CommandOutput::from_stdout(Vec::new())
    } else {
        CommandOutput::from_stdout(format!("{}\n", lines.join("\n")).into_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BufferSortOrder {
    Order,
    Name,
    Creation,
    Size,
}

impl BufferSortOrder {
    pub(super) fn parse(value: Option<&str>) -> Option<Self> {
        let Some(value) = value else {
            return Some(Self::Order);
        };

        if value.eq_ignore_ascii_case("order") {
            Some(Self::Order)
        } else if value.eq_ignore_ascii_case("creation") {
            Some(Self::Creation)
        } else if value.eq_ignore_ascii_case("size") {
            Some(Self::Size)
        } else if value.eq_ignore_ascii_case("name")
            || value.eq_ignore_ascii_case("title")
            || value.eq_ignore_ascii_case("activity")
            || value.eq_ignore_ascii_case("index")
            || value.eq_ignore_ascii_case("key")
            || value.eq_ignore_ascii_case("modifier")
        {
            Some(Self::Name)
        } else {
            None
        }
    }
}

pub(super) fn sort_buffer_entries(
    entries: &mut [BufferView<'_>],
    sort_order: BufferSortOrder,
    reversed: bool,
) {
    if sort_order == BufferSortOrder::Order {
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.order()));
    } else {
        entries.sort_by(|left, right| {
            let primary = match sort_order {
                BufferSortOrder::Name => left.name().cmp(right.name()),
                // `buffer_created` is second-granular in tmux; creation sorting
                // follows the underlying monotonic buffer order to stay stable
                // when several buffers are created within the same second.
                BufferSortOrder::Creation => left.order().cmp(&right.order()),
                BufferSortOrder::Size => left.size().cmp(&right.size()),
                BufferSortOrder::Order => std::cmp::Ordering::Equal,
            };
            primary.then_with(|| left.name().cmp(right.name()))
        });
    }

    if reversed {
        entries.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::{sort_buffer_entries, BufferSortOrder};
    use rmux_core::BufferStore;

    #[test]
    fn buffer_sort_order_accepts_tmux_aliases_case_insensitively() {
        assert_eq!(BufferSortOrder::parse(None), Some(BufferSortOrder::Order));
        assert_eq!(
            BufferSortOrder::parse(Some("NAME")),
            Some(BufferSortOrder::Name)
        );
        assert_eq!(
            BufferSortOrder::parse(Some("title")),
            Some(BufferSortOrder::Name)
        );
        assert_eq!(
            BufferSortOrder::parse(Some("KEY")),
            Some(BufferSortOrder::Name)
        );
        assert_eq!(
            BufferSortOrder::parse(Some("Creation")),
            Some(BufferSortOrder::Creation)
        );
        assert_eq!(
            BufferSortOrder::parse(Some("size")),
            Some(BufferSortOrder::Size)
        );
        assert_eq!(BufferSortOrder::parse(Some("bogus")), None);
    }

    #[test]
    fn creation_sort_uses_monotonic_order_when_created_timestamps_match() {
        for _ in 0..8 {
            let mut store = BufferStore::new();
            store.set(Some("zeta"), b"z".to_vec(), 50).unwrap();
            store.set(Some("alpha"), b"a".to_vec(), 50).unwrap();
            store.set(Some("middle"), b"m".to_vec(), 50).unwrap();

            let mut entries = store.entries();
            if entries
                .windows(2)
                .all(|pair| pair[0].created() == pair[1].created())
            {
                sort_buffer_entries(&mut entries, BufferSortOrder::Creation, false);
                let names = entries.iter().map(|entry| entry.name()).collect::<Vec<_>>();
                assert_eq!(names, vec!["zeta", "alpha", "middle"]);
                return;
            }
        }

        panic!("failed to create same-second buffers for creation-order test");
    }
}
