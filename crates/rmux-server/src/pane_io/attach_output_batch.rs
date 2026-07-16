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
        passthrough_sequences: Vec<u64>,
        close_after_render: bool,
        close_sequence: Option<u64>,
        sustained: bool,
    },
}

#[cfg(any(unix, windows))]
impl AttachOutputBatch {
    /// A same-source render refresh partitions output at its receiver start:
    /// the snapshot covers older bytes, while its new receiver owns bytes at
    /// and after the boundary. Only older passthrough side effects are absent
    /// from the snapshot and still need forwarding from an already dequeued
    /// batch.
    pub(super) fn covered_by_render_snapshot(self, before_sequence: u64) -> Self {
        let Self::Events {
            passthroughs,
            passthrough_sequences,
            close_sequence,
            ..
        } = self
        else {
            return self;
        };
        debug_assert_eq!(passthroughs.len(), passthrough_sequences.len());
        let (passthroughs, passthrough_sequences): (Vec<_>, Vec<_>) = passthroughs
            .into_iter()
            .zip(passthrough_sequences)
            .filter(|(_, sequence)| *sequence < before_sequence)
            .unzip();
        Self::Events {
            bytes: Vec::new(),
            passthroughs,
            passthrough_sequences,
            close_after_render: close_sequence.is_some_and(|sequence| sequence < before_sequence),
            close_sequence,
            sustained: false,
        }
    }
}

#[cfg(any(unix, windows))]
pub(super) fn collect_attach_output_batch(
    first_item: OutputCursorItem,
    receiver: Option<&mut PaneOutputReceiver>,
) -> AttachOutputBatch {
    collect_attach_output_batch_with_mode(first_item, receiver, ByteCollection::Collect)
}

#[cfg(any(unix, windows))]
pub(super) fn collect_attach_output_batch_metadata(
    first_item: OutputCursorItem,
    receiver: Option<&mut PaneOutputReceiver>,
) -> AttachOutputBatch {
    collect_attach_output_batch_with_mode(first_item, receiver, ByteCollection::Skip)
}

#[cfg(any(unix, windows))]
fn collect_attach_output_batch_with_mode(
    first_item: OutputCursorItem,
    receiver: Option<&mut PaneOutputReceiver>,
    byte_collection: ByteCollection,
) -> AttachOutputBatch {
    let mut batch = AttachOutputBatchBuilder::new(byte_collection);
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
    byte_collection: ByteCollection,
    saw_output_bytes: bool,
    bytes_seen: usize,
    events_seen: usize,
    close_after_render: bool,
    sustained: bool,
    bytes: Vec<u8>,
    passthroughs: Vec<TerminalPassthrough>,
    passthrough_sequences: Vec<u64>,
    close_sequence: Option<u64>,
}

#[cfg(any(unix, windows))]
impl AttachOutputBatchBuilder {
    fn new(byte_collection: ByteCollection) -> Self {
        Self {
            byte_collection,
            ..Self::default()
        }
    }

    fn push_first(&mut self, item: OutputCursorItem) -> Option<AttachOutputBatch> {
        self.push(item, GapLog::AlreadyLogged)
    }

    fn push_pending(&mut self, item: OutputCursorItem) -> Option<AttachOutputBatch> {
        self.push(item, GapLog::Log)
    }

    fn push(&mut self, item: OutputCursorItem, gap_log: GapLog) -> Option<AttachOutputBatch> {
        match item {
            OutputCursorItem::Event(event) => {
                let sequence = event.sequence();
                let byte_len = event.byte_len();
                let has_bytes = !event.is_empty();
                let has_passthroughs = !event.passthroughs().is_empty();
                let passthroughs = match self.byte_collection {
                    ByteCollection::Collect => {
                        let (bytes, passthroughs) = event.into_parts();
                        self.bytes.extend_from_slice(&bytes);
                        passthroughs
                    }
                    ByteCollection::Skip => event.into_passthroughs(),
                };
                self.passthrough_sequences
                    .extend(std::iter::repeat_n(sequence, passthroughs.len()));
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
                    self.close_sequence = Some(sequence);
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
                passthrough_sequences: self.passthrough_sequences,
                close_after_render: self.close_after_render,
                close_sequence: self.close_sequence,
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ByteCollection {
    #[default]
    Collect,
    Skip,
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

    use super::{
        collect_attach_output_batch, collect_attach_output_batch_metadata, AttachOutputBatch,
    };
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
            ..
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
    fn render_snapshot_keeps_only_pre_boundary_passthroughs_from_dequeued_batch() {
        let sender = pane_output_channel_with_limits(8, 1024);
        let mut receiver = sender.subscribe();
        sender.send_for_generation_with_passthroughs(
            None,
            b"covered".to_vec(),
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                b"Gf=100;covered".to_vec(),
            )],
        );
        sender.send_for_generation_with_passthroughs(
            None,
            b"after".to_vec(),
            vec![TerminalPassthrough::sixel(1, 1, b"q#0!10~".to_vec())],
        );
        sender.send(Vec::new());

        let first = receiver.try_recv().expect("first output event");
        let batch =
            collect_attach_output_batch(first, Some(&mut receiver)).covered_by_render_snapshot(1);

        let AttachOutputBatch::Events {
            bytes,
            passthroughs,
            passthrough_sequences,
            close_after_render,
            ..
        } = batch
        else {
            panic!("snapshot-covered output remains a passthrough batch");
        };
        assert!(bytes.is_empty(), "snapshot-covered bytes must not replay");
        assert_eq!(passthroughs.len(), 1);
        assert_eq!(passthrough_sequences, vec![0]);
        assert!(
            !close_after_render,
            "the replacement receiver still owns the close at the boundary"
        );
    }

    #[test]
    fn collect_metadata_batch_skips_output_byte_clones() {
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
        sender.send(b"two".to_vec());

        let first = receiver.try_recv().expect("first output event");
        let batch = collect_attach_output_batch_metadata(first, Some(&mut receiver));

        let AttachOutputBatch::Events {
            bytes,
            passthroughs,
            close_after_render,
            sustained,
            ..
        } = batch
        else {
            panic!("expected coalesced output batch");
        };
        assert!(
            bytes.is_empty(),
            "metadata batch must not clone raw output bytes"
        );
        assert_eq!(passthroughs.len(), 1);
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
                passthrough_sequences: Vec::new(),
                close_after_render: true,
                close_sequence: Some(1),
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
