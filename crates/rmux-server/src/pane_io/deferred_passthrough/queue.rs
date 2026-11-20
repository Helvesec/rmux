//! Ownership of the attach loop's deferred positioned passthroughs.
//!
//! The dispatcher beside this module decides *when* a queued payload may
//! become bytes. This module decides *whether the payload still belongs to
//! anything* — which pane-output generation produced it, and which frame
//! invalidations end its life. Keeping the two apart is what makes the
//! generation rule impossible to bypass: every emission path goes through the
//! dispatcher, and the dispatcher can only reach entries through this type.

use rmux_core::{TerminalPassthrough, TerminalPassthroughKind};
use tracing::warn;

use crate::pane_io::passthrough::passthrough_requires_cursor_position;

pub(in crate::pane_io) const DEFERRED_PASSTHROUGH_LIMIT: usize = 16;

/// The pane-output generation a deferred payload was produced under.
///
/// A respawn keeps the pane's `PaneOutputSender` allocation and only fences
/// its generation, so allocation identity cannot tell a dead process's image
/// from its replacement's: `shares_pane_source_with` reports the same source
/// on both sides of the boundary. The generation can, and it is the same value
/// `PaneOutputSender::reset_generation` stores.
///
/// `None` means the attach target had no live pane-output subscription at that
/// moment — before a target is opened, and after the exit path takes the
/// receiver. Losing a subscription is not evidence of a respawn, so a `None`
/// on either side never discards anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::pane_io) struct PaneOutputGeneration(Option<u64>);

impl PaneOutputGeneration {
    /// A target with no pane-output subscription to name a generation.
    pub(in crate::pane_io) const fn unsubscribed() -> Self {
        Self(None)
    }

    pub(in crate::pane_io) const fn subscribed(generation: u64) -> Self {
        Self(Some(generation))
    }

    /// Reports whether `self` is a strictly different generation from
    /// `queued`, which is the only case that proves the process behind a
    /// queued payload was replaced.
    ///
    /// Generations are only comparable within one `PaneOutputSender`
    /// allocation. `DeferredPassthroughQueue` upholds that precondition by
    /// unbinding whatever survives a target change, so a counter belonging to
    /// one pane is never weighed against another's.
    const fn supersedes(self, queued: Self) -> bool {
        match (self.0, queued.0) {
            (Some(current), Some(queued)) => current != queued,
            _ => false,
        }
    }
}

/// A passthrough waiting for a frame it may legitimately be drawn on, together
/// with the generation it was produced under.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DeferredPassthrough {
    generation: PaneOutputGeneration,
    passthrough: TerminalPassthrough,
}

fn passthrough_survives_source_invalidation(passthrough: &TerminalPassthrough) -> bool {
    passthrough.kind() == TerminalPassthroughKind::Clipboard
        && passthrough.clipboard_query_metadata().is_none()
}

/// The attach loop's bounded queue of passthroughs whose delivery is deferred.
///
/// Terminal graphics are forwarded live to attached clients; they are not part
/// of the text grid and are not replayed on later attaches. This queue delays a
/// passthrough for the two reasons that make the client's current frame the
/// wrong place to draw on: an rmux-owned overlay covers the pane, or DEC
/// synchronized output freezes the presentation screen so every render still
/// publishes the pre-interval content. It stays bounded so a busy
/// image-producing app cannot grow server memory through either path.
#[derive(Debug, Default)]
pub(in crate::pane_io) struct DeferredPassthroughQueue {
    entries: Vec<DeferredPassthrough>,
}

impl DeferredPassthroughQueue {
    pub(in crate::pane_io) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Queues `passthroughs` as work owned by `generation`.
    ///
    /// This is the only way an entry is born, so it is the only place a
    /// generation is assigned. A re-queue during a hold is deliberately not
    /// routed through here: re-queued work keeps the generation it was
    /// produced under, or a respawn landing mid-hold would silently adopt a
    /// dead process's image into its replacement.
    pub(in crate::pane_io) fn defer(
        &mut self,
        generation: PaneOutputGeneration,
        passthroughs: Vec<TerminalPassthrough>,
    ) {
        if passthroughs.is_empty() {
            return;
        }
        self.extend(
            passthroughs
                .into_iter()
                .map(|passthrough| DeferredPassthrough {
                    generation,
                    passthrough,
                }),
        );
    }

    /// Queues only side effects that do not belong to the invalidated pane.
    ///
    /// Filtering happens before `extend` enforces the bound, so doomed
    /// positioned work cannot evict an older source-independent clipboard
    /// write. Survivors are unbound for the same reason as target-change
    /// survivors: no foreign pane-output counter may be compared with them.
    pub(in crate::pane_io) fn defer_after_source_invalidation(
        &mut self,
        passthroughs: Vec<TerminalPassthrough>,
    ) {
        self.extend(
            passthroughs
                .into_iter()
                .filter(passthrough_survives_source_invalidation)
                .map(|passthrough| DeferredPassthrough {
                    generation: PaneOutputGeneration::unsubscribed(),
                    passthrough,
                }),
        );
    }

    /// Reports whether the queue still owns a passthrough whose rendered form
    /// carries an absolute cursor position, and therefore needs a converged
    /// render before it is released.
    pub(in crate::pane_io) fn holds_cursor_positioned(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| passthrough_requires_cursor_position(entry.passthrough.kind()))
    }

    /// Discards what a change of attach target invalidated.
    ///
    /// Everything anchored to the old pane's frame is gone; a non-query
    /// clipboard write is a pure side effect that is still worth delivering.
    /// What survives is unbound from its generation, because the next target
    /// may be a different `PaneOutputSender` whose counter means nothing to
    /// these entries.
    pub(in crate::pane_io) fn clear_if_target_changed(&mut self, target_changed: bool) {
        if !target_changed {
            return;
        }
        self.entries
            .retain(|entry| passthrough_survives_source_invalidation(&entry.passthrough));
        for entry in &mut self.entries {
            entry.generation = PaneOutputGeneration::unsubscribed();
        }
    }

    /// Takes everything renderable now, re-queuing what must keep waiting.
    ///
    /// Work whose generation `generation` supersedes is discarded first: the
    /// process that produced it has been replaced, so there is no frame left
    /// it may legitimately be drawn on. A non-query clipboard write outlives
    /// the process exactly as it outlives a target change.
    ///
    /// `hold_cursor_positioned` answers "is the frame this client is looking
    /// at the converged one?". While the presentation screen is frozen it is
    /// not, and rendering would anchor `Sixel`, `KittyGraphics` and `Raw`
    /// payloads onto a frame that still shows the pre-interval screen; the
    /// converged render that follows then repaints those cells and erases the
    /// image. Those kinds are therefore held until the interval ends.
    /// Clipboard writes and palette queries carry no cursor position and keep
    /// their immediate side effects — a palette query in particular must not
    /// be delayed past its correlation window.
    #[cfg(test)]
    pub(in crate::pane_io) fn take_renderable(
        &mut self,
        current_generation: PaneOutputGeneration,
        live_generation: PaneOutputGeneration,
        live_passthroughs: Vec<TerminalPassthrough>,
        hold_cursor_positioned: bool,
    ) -> Vec<TerminalPassthrough> {
        self.take_renderable_batches(
            current_generation,
            vec![(live_generation, live_passthroughs)],
            hold_cursor_positioned,
        )
    }

    /// The exit drain may collect several generation-coherent batches while a
    /// respawn races its polling loop. Keep every batch's own birth generation
    /// while preserving their FIFO order through the single dispatcher.
    pub(in crate::pane_io) fn take_renderable_batches(
        &mut self,
        current_generation: PaneOutputGeneration,
        live_batches: Vec<(PaneOutputGeneration, Vec<TerminalPassthrough>)>,
        hold_cursor_positioned: bool,
    ) -> Vec<TerminalPassthrough> {
        let mut entries = std::mem::take(&mut self.entries);
        entries.extend(
            live_batches
                .into_iter()
                .flat_map(|(generation, passthroughs)| {
                    passthroughs
                        .into_iter()
                        .map(move |passthrough| DeferredPassthrough {
                            generation,
                            passthrough,
                        })
                }),
        );
        Self::discard_superseded_generations(&mut entries, current_generation);
        if entries.is_empty() {
            return Vec::new();
        }
        if hold_cursor_positioned {
            let (positioned, immediate): (Vec<_>, Vec<_>) = entries
                .into_iter()
                .partition(|entry| passthrough_requires_cursor_position(entry.passthrough.kind()));
            self.extend(positioned.into_iter());
            return immediate
                .into_iter()
                .map(|entry| entry.passthrough)
                .collect();
        }
        entries.into_iter().map(|entry| entry.passthrough).collect()
    }

    /// Drops positioned work whose process has been replaced.
    ///
    /// `reset_generation` fences the ring and drops the sender's retained
    /// output, but it cannot reach this attach-loop-local queue; this is the
    /// same fence applied where the queue actually lives.
    fn discard_superseded_generations(
        entries: &mut Vec<DeferredPassthrough>,
        generation: PaneOutputGeneration,
    ) {
        if !entries
            .iter()
            .any(|entry| generation.supersedes(entry.generation))
        {
            return;
        }
        let before = entries.len();
        entries.retain(|entry| {
            !generation.supersedes(entry.generation)
                || (entry.passthrough.kind() == TerminalPassthroughKind::Clipboard
                    && entry.passthrough.clipboard_query_metadata().is_none())
        });
        warn!(
            discarded = before.saturating_sub(entries.len()),
            "discarded deferred terminal passthrough events queued against a replaced pane process"
        );
    }

    /// Appends entries and enforces the memory bound, dropping the oldest
    /// first so the newest frames survive.
    fn extend(&mut self, entries: impl Iterator<Item = DeferredPassthrough>) {
        self.entries.extend(entries);
        let overflow = self
            .entries
            .len()
            .saturating_sub(DEFERRED_PASSTHROUGH_LIMIT);
        if overflow > 0 {
            self.entries.drain(..overflow);
            warn!(
                dropped = overflow,
                retained = DEFERRED_PASSTHROUGH_LIMIT,
                "dropped deferred terminal passthrough events due to deferral safety limit"
            );
        }
    }

    #[cfg(test)]
    pub(in crate::pane_io) fn passthroughs(&self) -> Vec<TerminalPassthrough> {
        self.entries
            .iter()
            .map(|entry| entry.passthrough.clone())
            .collect()
    }

    #[cfg(test)]
    pub(in crate::pane_io) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(in crate::pane_io) fn generations(&self) -> Vec<PaneOutputGeneration> {
        self.entries.iter().map(|entry| entry.generation).collect()
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::{TerminalClipboardQuery, TerminalPaletteIndex, TerminalPassthrough};

    use super::{DeferredPassthroughQueue, PaneOutputGeneration, DEFERRED_PASSTHROUGH_LIMIT};

    fn image() -> TerminalPassthrough {
        TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;AAAA".to_vec())
    }

    fn clipboard_write() -> TerminalPassthrough {
        TerminalPassthrough::clipboard(b"\x1b]52;c;QQ==\x07".to_vec())
    }

    #[test]
    fn a_replaced_generation_discards_positioned_work_and_keeps_clipboard_writes() {
        let mut queue = DeferredPassthroughQueue::default();
        queue.defer(
            PaneOutputGeneration::subscribed(0),
            vec![image(), clipboard_write()],
        );

        let renderable = queue.take_renderable(
            PaneOutputGeneration::subscribed(1),
            PaneOutputGeneration::unsubscribed(),
            Vec::new(),
            false,
        );

        assert_eq!(renderable, vec![clipboard_write()]);
        assert!(queue.is_empty());
    }

    #[test]
    fn a_replaced_generation_discards_a_palette_query_with_its_correlation_window() {
        let mut queue = DeferredPassthroughQueue::default();
        queue.defer(
            PaneOutputGeneration::subscribed(0),
            vec![
                TerminalPassthrough::palette_query(TerminalPaletteIndex::from(7)),
                TerminalPassthrough::clipboard_query(
                    TerminalClipboardQuery::new("c", rmux_core::input::InputEndType::Bel),
                    b"\x1b]52;c;?\x07".to_vec(),
                ),
            ],
        );

        let renderable = queue.take_renderable(
            PaneOutputGeneration::subscribed(1),
            PaneOutputGeneration::unsubscribed(),
            Vec::new(),
            false,
        );

        assert!(renderable.is_empty(), "{renderable:?}");
        assert!(queue.is_empty());
    }

    #[test]
    fn a_held_entry_keeps_its_generation_across_a_re_queue() {
        let mut queue = DeferredPassthroughQueue::default();
        queue.defer(PaneOutputGeneration::subscribed(0), vec![image()]);

        // Two holds under the unchanged generation: a re-queue must not adopt
        // the entry into whatever generation is current when it is examined.
        for _ in 0..2 {
            let renderable = queue.take_renderable(
                PaneOutputGeneration::subscribed(0),
                PaneOutputGeneration::unsubscribed(),
                Vec::new(),
                true,
            );
            assert!(renderable.is_empty(), "{renderable:?}");
        }
        assert_eq!(
            queue.generations(),
            vec![PaneOutputGeneration::subscribed(0)]
        );

        let renderable = queue.take_renderable(
            PaneOutputGeneration::subscribed(1),
            PaneOutputGeneration::unsubscribed(),
            Vec::new(),
            true,
        );

        assert!(renderable.is_empty(), "{renderable:?}");
        assert!(
            queue.is_empty(),
            "a held image must not survive the respawn it was held across"
        );
    }

    #[test]
    fn live_work_is_owned_by_its_drain_generation() {
        let mut queue = DeferredPassthroughQueue::default();

        let renderable = queue.take_renderable(
            PaneOutputGeneration::subscribed(3),
            PaneOutputGeneration::subscribed(3),
            vec![image()],
            true,
        );

        assert!(renderable.is_empty(), "{renderable:?}");
        assert_eq!(
            queue.generations(),
            vec![PaneOutputGeneration::subscribed(3)]
        );
    }

    #[test]
    fn live_work_drained_before_a_reset_is_discarded_at_dispatch() {
        let mut queue = DeferredPassthroughQueue::default();

        let renderable = queue.take_renderable(
            PaneOutputGeneration::subscribed(4),
            PaneOutputGeneration::subscribed(3),
            vec![image(), clipboard_write()],
            false,
        );

        assert_eq!(renderable, vec![clipboard_write()]);
        assert!(queue.is_empty());
    }

    #[test]
    fn exit_batches_keep_fifo_while_fencing_each_birth_generation() {
        let mut queue = DeferredPassthroughQueue::default();
        let replacement_image = TerminalPassthrough::sixel(2, 3, b"q#1!8~".to_vec());

        let renderable = queue.take_renderable_batches(
            PaneOutputGeneration::subscribed(8),
            vec![
                (
                    PaneOutputGeneration::subscribed(7),
                    vec![image(), clipboard_write()],
                ),
                (
                    PaneOutputGeneration::subscribed(8),
                    vec![replacement_image.clone()],
                ),
            ],
            false,
        );

        assert_eq!(renderable, vec![clipboard_write(), replacement_image]);
        assert!(queue.is_empty());
    }

    #[test]
    fn an_unsubscribed_target_never_supersedes_or_is_superseded() {
        let mut queue = DeferredPassthroughQueue::default();
        queue.defer(PaneOutputGeneration::subscribed(0), vec![image()]);

        // The exit path takes the receiver before it drains, so the drain has
        // no generation to compare against. That is not evidence of a respawn.
        let renderable = queue.take_renderable(
            PaneOutputGeneration::unsubscribed(),
            PaneOutputGeneration::unsubscribed(),
            Vec::new(),
            false,
        );

        assert_eq!(renderable, vec![image()]);

        let mut queue = DeferredPassthroughQueue::default();
        queue.defer(PaneOutputGeneration::unsubscribed(), vec![image()]);

        let renderable = queue.take_renderable(
            PaneOutputGeneration::subscribed(9),
            PaneOutputGeneration::unsubscribed(),
            Vec::new(),
            false,
        );

        assert_eq!(renderable, vec![image()]);
    }

    #[test]
    fn a_target_change_unbinds_the_generation_of_what_it_keeps() {
        let mut queue = DeferredPassthroughQueue::default();
        queue.defer(
            PaneOutputGeneration::subscribed(7),
            vec![image(), clipboard_write()],
        );

        queue.clear_if_target_changed(true);

        assert_eq!(queue.passthroughs(), vec![clipboard_write()]);
        assert_eq!(
            queue.generations(),
            vec![PaneOutputGeneration::unsubscribed()],
            "a surviving entry must not be weighed against another pane's counter"
        );

        // The next pane's generation happens to differ; the clipboard write is
        // still delivered rather than discarded as a foreign generation.
        let renderable = queue.take_renderable(
            PaneOutputGeneration::subscribed(0),
            PaneOutputGeneration::unsubscribed(),
            Vec::new(),
            false,
        );
        assert_eq!(renderable, vec![clipboard_write()]);
    }

    #[test]
    fn invalidated_live_work_is_filtered_before_enforcing_the_bound() {
        let mut queue = DeferredPassthroughQueue::default();
        for _ in 0..DEFERRED_PASSTHROUGH_LIMIT {
            queue.defer(
                PaneOutputGeneration::unsubscribed(),
                vec![clipboard_write()],
            );
        }
        let retained = queue.passthroughs();

        queue.defer_after_source_invalidation(vec![image(); DEFERRED_PASSTHROUGH_LIMIT]);

        assert_eq!(queue.passthroughs(), retained);
        assert_eq!(queue.len(), DEFERRED_PASSTHROUGH_LIMIT);
    }

    #[test]
    fn the_bound_survives_generation_ownership() {
        let mut queue = DeferredPassthroughQueue::default();
        for index in 0..(DEFERRED_PASSTHROUGH_LIMIT as u32 * 2) {
            queue.defer(
                PaneOutputGeneration::subscribed(0),
                vec![TerminalPassthrough::sixel(0, index, b"q#0!10~".to_vec())],
            );
        }

        assert_eq!(queue.len(), DEFERRED_PASSTHROUGH_LIMIT);
        assert_eq!(
            queue.passthroughs().first(),
            Some(&TerminalPassthrough::sixel(
                0,
                DEFERRED_PASSTHROUGH_LIMIT as u32,
                b"q#0!10~".to_vec()
            ))
        );
    }
}
