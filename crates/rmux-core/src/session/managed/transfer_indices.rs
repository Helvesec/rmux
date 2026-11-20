use super::index_state::Links;
use super::*;

pub(super) struct TransferIndices(Vec<(SessionName, SessionId, Links, u32, Option<u32>)>);

impl TransferIndices {
    pub(super) fn capture(store: &ManagedSessionStore) -> Self {
        let mut names = store.sessions.keys().cloned().collect::<Vec<_>>();
        names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Self(
            names
                .into_iter()
                .map(|name| {
                    let session = &store.sessions[&name];
                    let links = store.index_links(&name).expect("existing session");
                    (
                        name,
                        session.id,
                        links,
                        session.active_window,
                        session.last_window,
                    )
                })
                .collect(),
        )
    }

    pub(super) fn finish(
        self,
        store: &ManagedSessionStore,
        maps: &HashMap<SessionName, BTreeMap<u32, u32>>,
    ) -> ManagedIndexChange {
        let mut sessions = Vec::new();
        for (name, session_id, before, previous_active, previous_last) in self.0 {
            let Some(session) = store.sessions.get(&name) else {
                continue;
            };
            let after = store.index_links(&name).expect("surviving session");
            let index_map = before
                .iter()
                .filter_map(|(&old, id)| {
                    let new = maps
                        .get(&name)
                        .and_then(|map| map.get(&old))
                        .copied()
                        .unwrap_or(old);
                    (after.get(&new) == Some(id)).then_some((old, new))
                })
                .collect();
            if before != after
                || previous_active != session.active_window
                || previous_last != session.last_window
            {
                sessions.push(ManagedSessionIndexChange {
                    session_id,
                    before,
                    after,
                    index_map,
                    previous_active,
                    active: session.active_window,
                    previous_last,
                    last: session.last_window,
                });
            }
        }
        ManagedIndexChange {
            sessions,
            removed_windows: Vec::new(),
        }
    }
}
