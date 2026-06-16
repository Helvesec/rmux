#[cfg(any(unix, windows))]
use rmux_core::events::OutputCursorItem;
#[cfg(any(unix, windows))]
use rmux_core::TerminalPassthrough;

#[cfg(any(unix, windows))]
use super::types::PaneOutputReceiver;
#[cfg(any(unix, windows))]
use super::wire::warn_pane_output_gap;

#[cfg(any(unix, windows))]
const ATTACH_OUTPUT_BATCH_LIMIT: usize = 64;
const ATTACH_SUSTAINED_OUTPUT_BATCH_BYTES: usize = 256 * 1024;
const ATTACH_SUSTAINED_OUTPUT_BATCH_EVENTS: usize = ATTACH_OUTPUT_BATCH_LIMIT;

#[cfg(any(unix, windows))]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AttachOutputBatch {
    Closed,
    Gap,
    Events {
        bytes: Vec<u8>,
        passthroughs: Vec<TerminalPassthrough>,
        close_after_render: bool,
        sustained: bool,
    },
}

#[cfg(any(unix, windows))]
pub(super) fn collect_attach_output_batch(
    first_item: OutputCursorItem,
    receiver: Option<&mut PaneOutputReceiver>,
) -> AttachOutputBatch {
    let mut batch = AttachOutputBatchBuilder::default();
    if let Some(result) = batch.push_first(first_item) {
        return result;
    }
    if let Some(receiver) = receiver {
        let limit = ATTACH_OUTPUT_BATCH_LIMIT.saturating_sub(1);
        let pending = receiver.try_recv_batch(limit);
        if pending.len() == limit {
            batch.mark_sustained();
        }
        for item in pending {
            if let Some(result) = batch.push_pending(item) {
                return result;
            }
            if batch.close_after_render {
                break;
            }
        }
    }
    batch.finish()
}

#[cfg(any(unix, windows))]
#[derive(Default)]
struct AttachOutputBatchBuilder {
    saw_output_bytes: bool,
    bytes_seen: usize,
    events_seen: usize,
    close_after_render: bool,
    sustained: bool,
    bytes: Vec<u8>,
    passthroughs: Vec<TerminalPassthrough>,
}

#[cfg(any(unix, windows))]
impl AttachOutputBatchBuilder {
    fn push_first(&mut self, item: OutputCursorItem) -> Option<AttachOutputBatch> {
        self.push(item, GapLog::AlreadyLogged)
    }

    fn push_pending(&mut self, item: OutputCursorItem) -> Option<AttachOutputBatch> {
        self.push(item, GapLog::Log)
    }

    fn push(&mut self, item: OutputCursorItem, gap_log: GapLog) -> Option<AttachOutputBatch> {
        match item {
            OutputCursorItem::Event(event) => {
                let byte_len = event.byte_len();
                let has_bytes = !event.is_empty();
                let (bytes, passthroughs) = event.into_parts();
                let has_passthroughs = !passthroughs.is_empty();
                self.bytes.extend_from_slice(&bytes);
                self.passthroughs.extend(passthroughs);
                if has_bytes {
                    self.saw_output_bytes = true;
                    self.events_seen = self.events_seen.saturating_add(1);
                    self.bytes_seen = self.bytes_seen.saturating_add(byte_len);
                    if self.bytes_seen >= ATTACH_SUSTAINED_OUTPUT_BATCH_BYTES
                        || self.events_seen >= ATTACH_SUSTAINED_OUTPUT_BATCH_EVENTS
                    {
                        self.mark_sustained();
                    }
                    return None;
                }
                if self.saw_output_bytes || has_passthroughs {
                    self.close_after_render = true;
                    return None;
                }
                Some(AttachOutputBatch::Closed)
            }
            OutputCursorItem::Gap(gap) => {
                if matches!(gap_log, GapLog::Log) {
                    warn_pane_output_gap(&gap);
                }
                Some(AttachOutputBatch::Gap)
            }
        }
    }

    fn finish(self) -> AttachOutputBatch {
        if self.saw_output_bytes || !self.passthroughs.is_empty() {
            AttachOutputBatch::Events {
                bytes: self.bytes,
                passthroughs: self.passthroughs,
                close_after_render: self.close_after_render,
                sustained: self.sustained,
            }
        } else {
            AttachOutputBatch::Closed
        }
    }

    fn mark_sustained(&mut self) {
        self.sustained = true;
    }
}

#[cfg(any(unix, windows))]
enum GapLog {
    AlreadyLogged,
    Log,
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use rmux_core::events::OutputCursorItem;
    use rmux_core::TerminalPassthrough;

    use super::{collect_attach_output_batch, AttachOutputBatch};
    use crate::pane_io::types::pane_output_channel_with_limits;

    #[test]
    fn collect_batch_accumulates_passthroughs_without_closing() {
        let sender = pane_output_channel_with_limits(8, 1024);
        let mut receiver = sender.subscribe();
        sender.send_for_generation_with_passthroughs(
            None,
            b"one".to_vec(),
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                b"Gf=100;one".to_vec(),
            )],
        );
        sender.send_for_generation_with_passthroughs(
            None,
            b"two".to_vec(),
            vec![TerminalPassthrough::sixel(1, 1, b"q#0!10~".to_vec())],
        );

        let first = receiver.try_recv().expect("first output event");
        let batch = collect_attach_output_batch(first, Some(&mut receiver));

        let AttachOutputBatch::Events {
            bytes,
            passthroughs,
            close_after_render,
            sustained,
        } = batch
        else {
            panic!("expected coalesced output batch");
        };
        assert_eq!(bytes, b"onetwo");
        assert_eq!(passthroughs.len(), 2);
        assert!(!close_after_render);
        assert!(!sustained);
    }

    #[test]
    fn collect_batch_renders_before_closing_when_close_follows_output() {
        let sender = pane_output_channel_with_limits(8, 1024);
        let mut receiver = sender.subscribe();
        sender.send(b"final".to_vec());
        sender.send(Vec::new());

        let first = receiver.try_recv().expect("first output event");
        let batch = collect_attach_output_batch(first, Some(&mut receiver));

        assert_eq!(
            batch,
            AttachOutputBatch::Events {
                bytes: b"final".to_vec(),
                passthroughs: Vec::new(),
                close_after_render: true,
                sustained: false,
            }
        );
    }

    #[test]
    fn collect_batch_marks_large_output_as_sustained() {
        let sender = pane_output_channel_with_limits(8, 128 * 1024);
        let mut receiver = sender.subscribe();
        sender.send(vec![b'x'; super::ATTACH_SUSTAINED_OUTPUT_BATCH_BYTES]);

        let first = receiver.try_recv().expect("first output event");
        let batch = collect_attach_output_batch(first, Some(&mut receiver));

        assert!(matches!(
            batch,
            AttachOutputBatch::Events {
                sustained: true,
                ..
            }
        ));
    }

    #[test]
    fn collect_batch_marks_full_batch_as_sustained() {
        let sender = pane_output_channel_with_limits(128, 1024);
        let mut receiver = sender.subscribe();
        for _ in 0..super::ATTACH_OUTPUT_BATCH_LIMIT {
            sender.send(b"x".to_vec());
        }

        let first = receiver.try_recv().expect("first output event");
        let batch = collect_attach_output_batch(first, Some(&mut receiver));

        assert!(matches!(
            batch,
            AttachOutputBatch::Events {
                sustained: true,
                ..
            }
        ));
    }

    #[test]
    fn collect_batch_reports_gap_before_rendering_partial_batch() {
        let sender = pane_output_channel_with_limits(1, 1024);
        let mut receiver = sender.subscribe();
        sender.send(b"first".to_vec());
        let first = receiver.try_recv().expect("first output event");
        sender.send(b"second".to_vec());
        sender.send(b"third".to_vec());

        assert!(matches!(
            collect_attach_output_batch(first, Some(&mut receiver)),
            AttachOutputBatch::Gap
        ));
    }

    #[test]
    fn collect_batch_treats_first_empty_event_as_closed() {
        assert!(matches!(
            collect_attach_output_batch(OutputCursorItem::Event(empty_event()), None),
            AttachOutputBatch::Closed
        ));
    }

    fn empty_event() -> rmux_core::events::OutputEvent {
        let sender = pane_output_channel_with_limits(1, 1024);
        let mut receiver = sender.subscribe();
        sender.send(Vec::new());
        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("empty event should be available");
        };
        event
    }

    #[test]
    fn collect_batch_marks_repeated_small_output_as_sustained() {
        let sender =
            pane_output_channel_with_limits(super::ATTACH_SUSTAINED_OUTPUT_BATCH_EVENTS + 1, 1024);
        let mut receiver = sender.subscribe();
        for _ in 0..super::ATTACH_SUSTAINED_OUTPUT_BATCH_EVENTS {
            sender.send(b"x".to_vec());
        }

        let first = receiver.try_recv().expect("first output event");
        let batch = collect_attach_output_batch(first, Some(&mut receiver));

        assert!(matches!(
            batch,
            AttachOutputBatch::Events {
                sustained: true,
                ..
            }
        ));
    }
}
