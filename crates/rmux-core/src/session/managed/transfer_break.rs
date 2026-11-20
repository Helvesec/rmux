use super::creation::checked_id;
use super::index_ops::{exhausted, first_gap, occupied};
use super::index_state::LocalState;
use super::*;
use crate::BreakPaneOptions;

/// Broken pane location, grouped index changes and source-link cleanup.
#[derive(Debug)]
#[must_use]
pub struct ManagedPaneBreak {
    /// Destination occurrence.
    pub window_index: u32,
    /// Canonical pane transfer, excluding the moved pane from cleanup.
    pub transfer: ManagedPaneTransfer,
}

impl ManagedSessionStore {
    /// Breaks into a new owner, or relocates an existing single-pane owner.
    /// Plans indices and checks allocation
    /// before extracting a pane; aliases are never copied into placements.
    pub fn break_pane(
        &mut self,
        source: &WindowTarget,
        pane_index: u32,
        destination: &SessionName,
        options: BreakPaneOptions,
    ) -> Result<ManagedPaneBreak, RmuxError> {
        self.break_pane_with_session_teardown(source, pane_index, destination, options, false)
    }

    /// Relocates the addressed winlink family, preserving other aliases, and keeps
    /// empty source metadata until the server reaches native runtime teardown.
    pub fn break_pane_for_runtime_transfer(
        &mut self,
        source: &WindowTarget,
        pane_index: u32,
        destination: &SessionName,
        options: BreakPaneOptions,
    ) -> Result<ManagedPaneBreak, RmuxError> {
        self.break_pane_with_session_teardown(source, pane_index, destination, options, true)
    }

    fn break_pane_with_session_teardown(
        &mut self,
        source: &WindowTarget,
        pane_index: u32,
        destination: &SessionName,
        options: BreakPaneOptions,
        defer_session_removal: bool,
    ) -> Result<ManagedPaneBreak, RmuxError> {
        let snapshot = super::transfer_indices::TransferIndices::capture(self);
        let source_link = self.target_link(source)?;
        let pane_id = self.transfer_pane(source, pane_index)?.id();
        let original = self.window(source)?;
        let source_id = original.id();
        let size = original.size();
        let single = original.pane_count() == 1;
        if !single && original.is_floating_pane_id(pane_id) {
            return Err(RmuxError::Server(
                "cannot break a floating pane out of a multi-pane window".to_owned(),
            ));
        }
        let source_flags =
            self.sessions[source.session_name()].winlink_alert_flags[&source.window_index()];
        let destination_members = self.session_group_members(destination);
        let same_group = destination_members.contains(source.session_name());
        let mut links = self.index_links(destination)?;
        self.validate_transfer_groups()?;
        let window_id = if single {
            source_id
        } else {
            WindowId::new(checked_id(self.next_window_id, "window")?)
        };
        let mut occurrences = links.keys().map(|&i| (i, i)).collect::<BTreeMap<_, _>>();
        let mut local = LocalState::from_session(&self.sessions[destination]);
        let shifting = options.after || options.before;
        let index = if shifting {
            let anchor = options.target_window_index.unwrap_or(local.active);
            if options.target_window_index.is_some() && !links.contains_key(&anchor) {
                return Err(invalid_window_target(destination, anchor));
            }
            let index = if options.before {
                anchor
            } else {
                anchor
                    .checked_add(1)
                    .ok_or_else(|| exhausted(destination))?
            };
            if links.last_key_value().is_some_and(|(&i, _)| i == u32::MAX) {
                return Err(exhausted(destination));
            }
            for (&old, new) in &mut occurrences {
                if old >= index {
                    *new = old + 1;
                }
            }
            links = links
                .into_iter()
                .map(|(i, id)| (occurrences[&i], id))
                .collect();
            local.active = occurrences[&local.active];
            local.last = local.last.map(|i| occurrences[&i]);
            // The shuffle moves the same occurrences, so their visits move with
            // them (`window.c:1758-1785`) instead of staying on indices that now
            // belong to another link.
            local.stack = crate::session::visit_history::carry_occurrences(
                &local.stack,
                &occurrences,
                local.active,
                |slot| occurrences.values().any(|moved| *moved == slot),
            );
            local.alerts = local
                .alerts
                .into_iter()
                .map(|(i, flags)| (occurrences[&i], flags))
                .collect();
            index
        } else {
            let index = match options.target_window_index {
                Some(i) => i,
                None => first_gap(&links, destination, 0)?,
            };
            if links.contains_key(&index)
                && !(single
                    && destination == source.session_name()
                    && index == source.window_index())
            {
                return Err(occupied(destination, index));
            }
            index
        };
        if single
            && !shifting
            && destination == source.session_name()
            && index == source.window_index()
        {
            let window = self
                .windows
                .resolve_mut(source_link)
                .expect("validated source");
            window.renumber_single_pane_to_zero();
            if let Some(name) = options.name {
                window.set_name(name);
            }
            return Ok(ManagedPaneBreak {
                window_index: index,
                transfer: ManagedPaneTransfer {
                    source_window_id: source_id,
                    target_window_id: source_id,
                    pane_id,
                    pane_index: 0,
                    destroyed_window: None,
                    change: snapshot.finish(self, &HashMap::new()),
                },
            });
        }
        // Capture original occurrences before adding the destination. A single-pane
        // break relocates its owner (the existing oracle contract), removing old aliases.
        let mut old_aliases = Vec::new();
        if single {
            let mut names = self.sessions.keys().cloned().collect::<Vec<_>>();
            names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            let source_members = self.session_group_members(source.session_name());
            for name in names {
                for (old, id) in self.index_links(&name)? {
                    let addressed_occurrence =
                        source_members.contains(&name) && old == source.window_index();
                    if id == source_id && (!defer_session_removal || addressed_occurrence) {
                        let shifted = if self.session_group_members(destination).contains(&name) {
                            occurrences[&old]
                        } else {
                            old
                        };
                        if !(self.session_group_members(destination).contains(&name)
                            && shifted == index)
                        {
                            old_aliases.push(WindowTarget::with_window(name.clone(), shifted));
                        }
                    }
                }
            }
        }
        links.insert(index, window_id);
        local.alerts.insert(
            index,
            if single {
                source_flags
            } else {
                AlertFlags::empty()
            },
        );
        let introduced = if single {
            None
        } else {
            let mut window = Window::new_with_initial_pane(size, pane_id, window_id);
            if let Some(name) = &options.name {
                window.set_name(name.clone());
            }
            Some(window)
        };
        // Index commit is the last fallible step, and leaves the original pane
        // untouched on rejection. No allocation counter is advanced until it commits.
        // The addressed session's own state was already shifted above; a peer
        // only gets resynchronised, so its visits keep their slots.
        let history = super::index_state::identity_map(&links);
        let committed = self.commit_indices(
            destination,
            links,
            super::index_state::IndexMaps {
                occurrences: occurrences.clone(),
                history,
                // A break can shift the destination's slots and can address a
                // group, neither of which leaves the links where they were.
                link_order: crate::window_state_store::LinkOrderPlan::Rebuilt,
            },
            local,
            shifting.then_some(&occurrences),
            introduced,
        )?;
        debug_assert!(committed.removed_windows.is_empty());
        if single && !same_group {
            // New destination occurrences inherit the addressed source's alerts.
            // Existing aliases retain the occurrence-local flags staged above.
            for member in &destination_members {
                if member != destination {
                    self.metadata_mut(member)
                        .expect("validated destination peer")
                        .winlink_alert_flags
                        .insert(index, source_flags);
                }
            }
        }
        if !single {
            self.next_window_id += 1;
        }
        let source_index = if self
            .session_group_members(destination)
            .iter()
            .any(|name| name == source.session_name())
        {
            occurrences[&source.window_index()]
        } else {
            source.window_index()
        };
        let source_link = WindowLink {
            window_index: source_index,
            ..source_link
        };
        let target = WindowTarget::with_window(destination.clone(), index);
        let destroyed_window = if single {
            let window = self
                .windows
                .resolve_mut(source_link)
                .expect("surviving owner");
            window.renumber_single_pane_to_zero();
            if let Some(name) = options.name {
                window.set_name(name);
            }
            let mut removal = ManagedWindowRemoval {
                window_id: source_id,
                window: None,
                links: Vec::new(),
                removed_sessions: Vec::new(),
            };
            if !options.detached {
                // Within a group this is a relocation, not selection followed by
                // destruction. Compute history before selection overwrites it with
                // the source slot; ordinary alias removal still handles other sessions.
                let relocated = same_group.then(|| {
                    let mut links = self.index_links(destination).expect("destination");
                    links.remove(&source_index);
                    let mut state = LocalState::from_session(&self.sessions[destination]);
                    state.move_slot(&links, source_index, index, true);
                    state
                });
                self.select_window(&target).expect("destination");
                if let Some(state) = relocated {
                    self.metadata_mut(destination)
                        .expect("destination")
                        .last_window = state.last;
                }
            }
            // A peer's selection and its alerts follow the relocated
            // occurrence, even when detached source navigation chooses another
            // slot. Its visits do not: they stay on the slots it already had
            // (`session.c:663-678`). Do this before unlink so ordinary removal
            // cannot substitute its previous-window fallback.
            if defer_session_removal && same_group {
                for member in &destination_members {
                    if member == destination {
                        continue;
                    }
                    let peer = self.metadata_mut(member).expect("validated peer");
                    // Insertion carried this occurrence's alerts to the temporary
                    // source slot. Relocate them before unlink removes that slot;
                    // other aliases of the same owner keep their own flags.
                    let flags = peer.winlink_alert_flags[&source_index];
                    peer.winlink_alert_flags.insert(index, flags);
                    if peer.active_window == source_index {
                        peer.active_window = index;
                    }
                    // The visit stays on the slot this peer knew: the link is
                    // unlinked right after, and the ordinary removal below drops
                    // that slot and derives the return from what is left.
                    peer.last_window = crate::session::visit_history::live_head(
                        &peer.window_stack,
                        peer.active_window,
                    );
                }
            }
            for alias in old_aliases {
                let link = self.target_link(&alias).expect("original alias");
                let (owner, session) = self.remove_occurrence_with_session_teardown(
                    &alias,
                    link,
                    defer_session_removal,
                    DetachSource::Unmodelled,
                );
                debug_assert!(owner.is_none(), "destination retains canonical owner");
                removal.links.push(link);
                removal.removed_sessions.extend(session);
            }
            Some(removal)
        } else {
            let (source_window, destination_window) =
                self.windows.transfer_pair_mut(source_id, window_id);
            crate::session::pane_transfer_shared::apply_break_into_window(
                source_window,
                pane_index,
                destination_window,
            );
            if !options.detached {
                self.select_window(&target).expect("introduced occurrence");
            }
            None
        };
        let mut maps = HashMap::new();
        for member in self.session_group_members(destination) {
            let mut map = occurrences.clone();
            if single && same_group {
                map.insert(source.window_index(), index);
            }
            maps.insert(member, map);
        }
        let change = snapshot.finish(self, &maps);
        Ok(ManagedPaneBreak {
            window_index: index,
            transfer: ManagedPaneTransfer {
                source_window_id: source_id,
                target_window_id: window_id,
                pane_id,
                pane_index: 0,
                destroyed_window,
                change,
            },
        })
    }
}
