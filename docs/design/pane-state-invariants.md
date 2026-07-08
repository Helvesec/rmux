# Pane-State Journal Invariants

These invariants govern `crates/rmux-server/src/pane_state_journal.rs`,
`crates/rmux-server/src/handler_pane_state.rs`, and every code path that
creates, destroys, or respawns panes. Any change to the journal or to a
pane-destruction path must keep them true, and must keep the model-based
test `invariants_hold_under_random_push_evict_close_read_interleavings`
(in `pane_state_journal.rs`) passing — extend the model when adding
behavior, never weaken the assertions.

## I1 — Cursor monotonicity

A subscription cursor advances only over events actually delivered to (or
explicitly rebased past, via Lag) that subscription, and never advances past
an undelivered terminal `Closed` record for its pane.

## I2 — Closed delivery

Every pane-destruction path journals a terminal `Closed` exactly once per
pane lifetime: `kill-pane`, `kill-window`, `kill-session`, SDK `PaneKill`,
`respawn-pane`/`respawn-window` (including destroyed sibling panes and error
paths), `link-window`/`move-window`/`unlink-window` replacement, session
close, and server error paths that remove panes. Every live subscription
eventually observes `Closed`: if the record was evicted before delivery, the
read path synthesizes the terminal event. A read on a closed subscription
never returns an error such as `pane N not found` and never ends silently.

## I3 — Terminal semantics

After `Closed` is delivered to a subscription, no further events are
delivered to it. A subscription created after a `DiedKept` close still
receives a `Closed` when the kept-dead pane is later killed: close dedup is
keyed per subscription-visible lifetime (generation), not per `PaneId` alone.

## I4 — Respawn ordering

Reopening a pane (clearing its closed marker) is atomic with respect to the
respawned process's exit record: both paths take the journal lock in a
documented order, and a respawned command that exits immediately must not
have its `Closed` swallowed by a stale closed marker.

## I5 — Lag correctness

Lag is evaluated per subscription over matching events (pane id + include
mask) — for closed subscriptions too. A subscriber never silently misses a
matching event: either the event is delivered in order, or a `Lag` signal
(leading to a snapshot rebase or a synthesized `Closed`) is returned first.
The terminal `Closed` itself is synthesizable and therefore never a reason
to report `Lag` on its own.

## I6 — Bounded memory without breaking I2/I3

`closed_panes`, `evicted_revisions`, and the subscription map are bounded,
but eviction of state that I2/I3 still need degrades to synthesis-on-read —
never to destruction of an undelivered `Closed` and never to a duplicate
`Closed`.

## I7 — Hot-path cost

No O(capacity) scan per push while holding the journal mutex; per-push work
is at worst O(matching subscribers), and pruning is amortized.

## I8 — Locking

The handler state lock and the journal mutex follow a documented order
(state lock is never acquired while the journal mutex is held), and neither
is held across an `.await`.
