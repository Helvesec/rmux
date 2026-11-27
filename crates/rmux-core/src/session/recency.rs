#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static NEXT_SESSION_RECENCY: AtomicU64 = AtomicU64::new(0);

/// Opaque process-local total order over session recency events.
///
/// Public session times are whole Unix seconds, so `session_created` and
/// `session_activity` cannot order two events inside the same second. tmux
/// compares full `timeval` values instead, which is why it ranks the last of
/// three same-second sessions first. This token restores that total order: it
/// is an internal workspace key, never serialized, rendered or compared across
/// processes, so it adds no public format, option or wire value of its own.
///
/// It does not follow that the surrounding behavior is unchanged. One of the
/// events that mints a token — an attached client's accepted interaction —
/// also advances the public `activity_at` through [`Session::touch_activity`],
/// which `#{session_activity}` and the `list-sessions` payload publish. That
/// is a deliberate, tmux-measured change to observable output; only the token
/// itself is invisible.
///
/// Only a lifetime or accepted-interaction event mints a new token. Renaming a
/// session, synchronizing grouped windows, pane output and detaching all leave
/// the existing token in place.
///
/// The order is total only while no two stored sessions hold the same token.
/// `Session` is publicly cloneable, so a session can arrive at a store already
/// carrying a live token; [`SessionStore::insert_existing_session`] re-mints on
/// that collision, because readers break an equal-token tie differently — some
/// on the creation id, some on the name — and would disagree.
///
/// [`Session::touch_activity`]: crate::Session::touch_activity
/// [`SessionStore::insert_existing_session`]: crate::SessionStore::insert_existing_session
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionRecency(u64);

impl SessionRecency {
    /// Mints the next token in the process-local total order.
    pub(super) fn next() -> Self {
        // `fetch_update` rather than `fetch_add` so exhaustion aborts instead
        // of wrapping a live ordering key back behind existing sessions.
        let sequence = NEXT_SESSION_RECENCY
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .expect("session recency sequence exhausted");
        Self(sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::SessionRecency;

    #[test]
    fn atomic64_recency_is_unique_across_threads() {
        let mut tokens = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| (0..128).map(|_| SessionRecency::next()).collect::<Vec<_>>())
                })
                .collect();
            threads
                .into_iter()
                .flat_map(|thread| {
                    let tokens = thread.join().expect("recency thread");
                    assert!(tokens.windows(2).all(|pair| pair[0] < pair[1]));
                    tokens
                })
                .collect::<Vec<_>>()
        });
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), 512);
    }
}
