//! The windows a session really visited, and the only writes that touch them.
//!
//! A visit belongs to a winlink occurrence, never to the shared `WindowId`: two
//! sessions linking the same window keep their own histories, and a slot that
//! another occurrence takes over does not inherit the visit of the one it
//! replaced. Every site that moves, removes or selects an occurrence goes
//! through the three functions here, so nothing writes this state ad hoc.
//!
//! The primary is `s->lastw`: `session_set_current` takes the target out and
//! pushes the window being left onto the head (`session.c:475-487`),
//! `winlink_stack_remove` drops an occurrence that goes away (`window.c:255-260`,
//! called from `session.c:350` and `spawn.c:135`), and a renumber keeps the
//! occurrences and only moves their indices (`session.c:724-731`).

/// Records a real selection: the window being left goes to the head, the target
/// leaves the history, and no window appears twice.
pub(crate) fn record_visit(stack: &mut Vec<u32>, left: u32, selected: u32) {
    if left == selected {
        return;
    }
    stack.retain(|visited| *visited != selected);
    stack.insert(0, left);
}

/// Drops the occurrence that no longer exists.
pub(crate) fn forget_occurrence(stack: &mut Vec<u32>, window_index: u32) {
    stack.retain(|visited| *visited != window_index);
}

/// Carries the history onto the slots an explicit occurrence map defines.
///
/// An entry whose occurrence moved follows it, an entry whose occurrence is gone
/// is dropped, the active window is never in the history, and no window appears
/// twice. `survives` answers for the slots that exist afterwards.
pub(crate) fn carry_occurrences(
    stack: &[u32],
    occurrences: &impl OccurrenceMap,
    active: u32,
    survives: impl Fn(u32) -> bool,
) -> Vec<u32> {
    let mut carried = Vec::with_capacity(stack.len());
    for visited in stack {
        let Some(slot) = occurrences.moved_to(*visited) else {
            continue;
        };
        if slot == active || !survives(slot) || carried.contains(&slot) {
            continue;
        }
        carried.push(slot);
    }
    carried
}

/// The window a return should take: the live head of the history.
///
/// `session_last` takes the first entry of the stack that survived
/// (`session.c:461-471`), so this is what `last_window` has to agree with after a
/// mutation touched it. A history that names nothing leaves none, and the active
/// window is never proposed.
pub(crate) fn live_head(stack: &[u32], active: u32) -> Option<u32> {
    stack.iter().copied().find(|head| *head != active)
}

/// Where an occurrence went, by the explicit map its caller validated.
pub(crate) trait OccurrenceMap {
    /// The slot this occurrence now has, or nothing when it no longer exists.
    fn moved_to(&self, window_index: u32) -> Option<u32>;
}

impl OccurrenceMap for std::collections::BTreeMap<u32, u32> {
    fn moved_to(&self, window_index: u32) -> Option<u32> {
        self.get(&window_index).copied()
    }
}
