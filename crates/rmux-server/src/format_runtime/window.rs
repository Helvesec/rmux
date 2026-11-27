use crate::client_format::ClientLoopSnapshot;
use rmux_core::formats::{FormatVariable, FormatVariables};
use rmux_core::{AlertFlags, Window, WINLINK_ACTIVITY, WINLINK_BELL, WINLINK_SILENCE};
use rmux_proto::SessionName;

use super::{bool_string, RuntimeFormatContext};

impl RuntimeFormatContext<'_> {
    /// The sessions where this window is the current one, in link order.
    ///
    /// tmux walks the window's links and keeps each one whose session has it as
    /// `curw`, joining their names in the order the links were added
    /// (`format.c:718-767`, `window.c:185-192`); with no window link at all it
    /// answers nothing. Read concrete winlinks before their indices are lost
    /// in the runtime's membership aggregation. If their chronology is unknown
    /// after a rebuild, follow the fallback `#{window_linked_sessions_list}`
    /// takes.
    pub(super) fn window_active_session_names(&self) -> Option<Vec<SessionName>> {
        let session = self.session?;
        let window_index = self.window_index?;
        let state = self.state?;
        Some(
            state
                .sessions
                .window_active_sessions_in_link_order(&rmux_proto::WindowTarget::with_window(
                    session.name().clone(),
                    window_index,
                ))
                .unwrap_or_else(|| {
                    state.window_active_sessions_presentation_fallback(session.name(), window_index)
                }),
        )
    }

    pub(super) fn session_group_name(&self) -> Option<SessionName> {
        let session = self.session?;
        self.session_store
            .and_then(|store| store.session_group_name(session.name()).cloned())
            .or_else(|| session.group_name().cloned())
    }

    pub(super) fn session_group_members(&self) -> Vec<SessionName> {
        let Some(session) = self.session else {
            return Vec::new();
        };
        if self.session_group_name().is_none() {
            return Vec::new();
        }
        self.session_store
            .map(|store| store.session_group_members(session.name()))
            .unwrap_or_default()
    }

    /// The group's members in the order the group took them.
    ///
    /// tmux answers `#{session_group_list}` from the group's own TAILQ, where
    /// every member was appended as it joined (`format.c:1075-1090`,
    /// `session.c:543-548`), and a group is created with its TEMPLATE before the
    /// session that asked for it (`cmd-new-session.c:316-325`). This runtime
    /// keeps no such list, but it does keep a stable session identity, and under
    /// the invariant this store maintains - the only ways into a group are a
    /// singleton that declares one, or an existing template joined by a session
    /// whose identity is minted after it (`managed/creation.rs:93-183`) - those
    /// identities rise in exactly the order the members joined.
    ///
    /// That invariant is what this reads, and it is narrower than the sentence
    /// above: a session's birth and its JOIN are the same event HERE, and this
    /// helper does not assert they are the same everywhere. A member removed and
    /// created again gets a new identity, which is why it comes back at the
    /// tail - as tmux answers.
    ///
    /// Presentation only: `session_group_members` stays sorted by name, so every
    /// other reading of membership keeps the order it had.
    pub(super) fn session_group_list_names(&self) -> Vec<SessionName> {
        let mut members = self.session_group_members();
        let Some(store) = self.session_store else {
            return members;
        };
        // A stable sort, so a member this store cannot resolve keeps the name
        // order it arrived in rather than jumping to the front.
        members.sort_by_key(|session_name| {
            store
                .session(session_name)
                .map_or(u32::MAX, |session| session.id().as_u32())
        });
        members
    }

    /// The clients attached to any session of this session's group, in the
    /// order the server created them.
    ///
    /// tmux keeps ONE global client list and inserts each new client at its tail
    /// (`server-client.c:327`); `session_group_attached_list` walks that list and
    /// keeps every client whose session is in the group, so the answer is ordered
    /// by client creation and not by session, and not by name
    /// (`format.c:1093-1131`). `session_group_attached_count` counts that same
    /// set, as the sum of `s->attached` over the group's sessions
    /// (`session.c:580-591`).
    ///
    /// The same snapshots the client loop already reads are filtered here; there
    /// is no second register of clients and nothing about an attachment moves.
    ///
    /// `None` says this runtime cannot answer for the group: the session is in
    /// no group, or no caller handed it the server's clients.
    pub(super) fn session_group_attached_clients(&self) -> Option<Vec<&ClientLoopSnapshot>> {
        let session = self.session?;
        self.session_group_name()?;
        let snapshots = self.client_loop_snapshots?;
        let mut members = self.session_group_members();
        if members.is_empty() {
            // A group always holds the session it is read from, even where this
            // runtime has no store with which to enumerate the others.
            members.push(session.name().clone());
        }
        let mut clients = snapshots
            .iter()
            .filter(|client| {
                client.is_attached()
                    && members
                        .iter()
                        .any(|member| member.as_str() == client.bindings.session.as_str())
            })
            .collect::<Vec<_>>();
        // The same order `sort_client_loop_snapshots` gives, taken here so the
        // answer does not depend on the caller having sorted first.
        clients.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.name().cmp(right.name()))
        });
        Some(clients)
    }

    /// How many clients the group answers for.
    ///
    /// When the client list never reached this runtime the count it does have is
    /// what it keeps: this session's own attachment, exactly as before this
    /// helper existed. A group answer is never invented from a local count.
    pub(super) fn session_group_attached_count(&self) -> Option<usize> {
        self.session_group_name()?;
        Some(match self.session_group_attached_clients() {
            Some(clients) => clients.len(),
            None => self.session_attached_count(),
        })
    }

    /// How many clients are looking at THIS window right now.
    ///
    /// tmux answers nothing at all without a winlink, and otherwise walks its
    /// ONE global client list, skips every client with no session, and counts
    /// each remaining client whose `session->curw->window` is this format's
    /// window (`format.c:772-795`). There is no group restriction, no winlink
    /// multiplier and no branch on the session the format is being read from -
    /// which is exactly why a DETACHED session can answer for a window its
    /// peer's client is viewing.
    ///
    /// The same snapshots the client loop already captured are read here; there
    /// is no second register of clients, nothing about an attachment moves, and
    /// no alias, group, name or local index takes part. A client whose session
    /// this store cannot resolve any more is skipped, as the primary skips a
    /// client with no session; a session that holds this window under several
    /// links still contributes its clients ONCE, because what is compared is
    /// the canonical window identity and not a link.
    ///
    /// `Some(0)` is a complete answer: the target window is known, the
    /// inventory was handed over, and no client of any session is on it.
    /// `None` says the inputs a global count needs never reached this runtime -
    /// no target window, no session store, or no client list - so its reader
    /// must keep what it answered before rather than read a zero as authority.
    pub(super) fn window_active_client_count(&self) -> Option<usize> {
        let window = self.window?;
        let store = self.session_store?;
        let snapshots = self.client_loop_snapshots?;
        Some(
            snapshots
                .iter()
                .filter(|client| client.is_attached())
                .filter(|client| {
                    // Resolved by the store's OWN name, never by rebuilding a
                    // SessionName from display text.
                    store
                        .iter()
                        .find(|session| session.name().as_str() == client.bindings.session.as_str())
                        .is_some_and(|session| session.window().id() == window.id())
                })
                .count(),
        )
    }

    /// The NAMES of the clients looking at THIS window, in the order the server
    /// created them.
    ///
    /// The same walk `window_active_client_count` counts: tmux answers nothing
    /// without a winlink, and otherwise goes through its ONE global client
    /// list, skips every client with no session, and appends the name of each
    /// remaining client whose `session->curw->window` is this format's window,
    /// joined by a single comma (`format.c:797-831`). Every new client joins
    /// that list at the tail (`server-client.c:327`), which is the order given
    /// back here - the same `order` then `name` ordering the group list already
    /// uses, taken so the answer does not depend on the caller having sorted
    /// first.
    ///
    /// A window held under several links still names a client ONCE, because
    /// what is compared is the canonical window identity and not a link; two
    /// clients of one session are two names.
    ///
    /// `Some` of an empty list is a complete answer - the window is known, the
    /// inventory arrived and nobody is on it - and renders empty, as the
    /// primary renders its empty buffer. `None` says the inputs never reached
    /// this runtime, and its reader keeps the answer it had.
    pub(super) fn window_active_client_names(&self) -> Option<Vec<String>> {
        let window = self.window?;
        let store = self.session_store?;
        let snapshots = self.client_loop_snapshots?;
        let mut viewing = snapshots
            .iter()
            .filter(|client| client.is_attached())
            .filter(|client| {
                // Resolved by the store's OWN name, never by rebuilding a
                // SessionName from display text.
                store
                    .iter()
                    .find(|session| session.name().as_str() == client.bindings.session.as_str())
                    .is_some_and(|session| session.window().id() == window.id())
            })
            .collect::<Vec<_>>();
        viewing.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.name().cmp(right.name()))
        });
        Some(
            viewing
                .into_iter()
                .map(|client| client.name().to_owned())
                .collect(),
        )
    }

    pub(super) fn session_attached_count(&self) -> usize {
        self.base
            .format_value(FormatVariable::SessionAttached)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0)
    }

    pub(super) fn window_flags(&self) -> Option<String> {
        Some(self.printable_window_flags(true))
    }

    pub(super) fn window_linked(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        Some(bool_string(
            self.state?.window_winlink_count(session_name, window_index) > 1,
        ))
    }

    pub(super) fn window_linked_sessions(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        Some(
            self.state?
                .window_linked_session_group_count(session_name, window_index)
                .to_string(),
        )
    }

    pub(super) fn window_linked_sessions_list(&self) -> Option<String> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        Some(
            self.state?
                .window_linked_sessions_presentation_list(session_name, window_index)
                .into_iter()
                .map(|session_name| session_name.to_string())
                .collect::<Vec<_>>()
                .join(","),
        )
    }

    fn window_alert_flags(&self) -> AlertFlags {
        self.session
            .zip(self.window_index)
            .map(|(session, window_index)| session.winlink_alert_flags(window_index))
            .unwrap_or_else(AlertFlags::empty)
    }

    pub(super) fn printable_window_flags(&self, escape_activity: bool) -> String {
        let active = self
            .base
            .format_value(FormatVariable::WindowActive)
            .is_some_and(|value| value == "1");
        let last = self
            .base
            .format_value(FormatVariable::WindowLastFlag)
            .is_some_and(|value| value == "1");
        let zoomed = self.window.is_some_and(Window::is_zoomed);
        let alerts = self.window_alert_flags();

        let mut flags = String::new();
        if alerts.contains(WINLINK_ACTIVITY) {
            if escape_activity {
                flags.push_str("##");
            } else {
                flags.push('#');
            }
        }
        if alerts.contains(WINLINK_BELL) {
            flags.push('!');
        }
        if alerts.contains(WINLINK_SILENCE) {
            flags.push('~');
        }
        if active {
            flags.push('*');
        } else if last {
            flags.push('-');
        }
        if zoomed {
            flags.push('Z');
        }
        flags
    }

    pub(super) fn session_alert(&self) -> Option<String> {
        let session = self.session?;
        let flags = session.session_alert_flags();
        let mut value = String::new();
        if flags.contains(WINLINK_ACTIVITY) {
            value.push('#');
        }
        if flags.contains(WINLINK_BELL) {
            value.push('!');
        }
        if flags.contains(WINLINK_SILENCE) {
            value.push('~');
        }
        Some(value)
    }

    pub(super) fn session_alerts(&self) -> Option<String> {
        let session = self.session?;
        let alerts = session
            .alerted_window_indexes()
            .into_iter()
            .filter_map(|window_index| {
                let flags = session.winlink_alert_flags(window_index);
                if flags.is_empty() {
                    return None;
                }

                let mut value = window_index.to_string();
                if flags.contains(WINLINK_ACTIVITY) {
                    value.push('#');
                }
                if flags.contains(WINLINK_BELL) {
                    value.push('!');
                }
                if flags.contains(WINLINK_SILENCE) {
                    value.push('~');
                }
                Some(value)
            })
            .collect::<Vec<_>>();
        Some(alerts.join(","))
    }

    pub(super) fn session_flag(&self, flag: AlertFlags) -> Option<String> {
        self.session
            .map(|session| bool_string(session.session_alert_flags().contains(flag)))
    }

    pub(super) fn window_flag(&self, flag: AlertFlags) -> Option<String> {
        self.window_index
            .map(|_| bool_string(self.window_alert_flags().contains(flag)))
    }
}
