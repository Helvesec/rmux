//! OSC 8 hyperlink storage matching tmux's inner-ID model.

use std::collections::{BTreeMap, VecDeque};

use crate::vis::{encode_str, VisFlags};

/// Maximum retained hyperlink entries, matching tmux.
pub(crate) const MAX_HYPERLINKS: usize = 5000;

/// One stored hyperlink entry keyed by its internal `inner_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HyperlinkEntry {
    /// The grid-stored hyperlink identifier.
    pub inner_id: u32,
    /// The application-provided internal ID, when present.
    pub internal_id: String,
    /// The terminal-facing external ID assigned by rmux.
    pub external_id: String,
    /// The hyperlink target URI.
    pub uri: String,
}

/// Hyperlink table shared by every grid belonging to one screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Hyperlinks {
    next_inner: u32,
    next_external: u64,
    order: VecDeque<u32>,
    by_inner: BTreeMap<u32, HyperlinkEntry>,
    by_uri: BTreeMap<(String, String), u32>,
}

impl Hyperlinks {
    /// Creates an empty hyperlink table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_inner: 1,
            next_external: 1,
            order: VecDeque::new(),
            by_inner: BTreeMap::new(),
            by_uri: BTreeMap::new(),
        }
    }

    /// Stores or resolves a hyperlink and returns its inner ID.
    ///
    /// Anonymous hyperlinks (`internal_id == None` or empty) are intentionally
    /// never deduplicated.
    pub fn put(&mut self, uri: &str, internal_id: Option<&str>) -> u32 {
        let flags = VisFlags {
            octal: true,
            cstyle: true,
            ..VisFlags::default()
        };
        let uri = encode_str(uri, flags);
        let internal_id = encode_str(internal_id.unwrap_or_default(), flags);
        if !internal_id.is_empty() {
            let key = (internal_id.clone(), uri.clone());
            if let Some(inner_id) = self.by_uri.get(&key) {
                return *inner_id;
            }
        }

        let inner_id = self.next_inner;
        self.next_inner = self.next_inner.saturating_add(1);

        let entry = HyperlinkEntry {
            inner_id,
            internal_id: internal_id.clone(),
            external_id: format!("tmux{:X}", self.next_external),
            uri: uri.clone(),
        };
        self.next_external = self.next_external.saturating_add(1);

        if !entry.internal_id.is_empty() {
            self.by_uri.insert(
                (entry.internal_id.clone(), entry.uri.clone()),
                entry.inner_id,
            );
        }
        self.order.push_back(entry.inner_id);
        self.by_inner.insert(entry.inner_id, entry);
        self.enforce_limit();
        inner_id
    }

    /// Returns a stored hyperlink by inner ID.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn get(&self, inner_id: u32) -> Option<&HyperlinkEntry> {
        self.by_inner.get(&inner_id)
    }

    /// Removes all stored hyperlinks while keeping the table allocation.
    pub fn reset(&mut self) {
        self.order.clear();
        self.by_inner.clear();
        self.by_uri.clear();
    }

    /// Copies only metadata that fits the supplied recovery budgets.
    ///
    /// Grid cells keep their original inner IDs, so retained entries must keep
    /// those IDs as well. Entries that do not fit are omitted; renderers then
    /// treat the corresponding cell as unlinked instead of cloning or
    /// serializing an attacker-sized URI.
    pub(crate) fn clone_bounded(
        &self,
        max_entry_bytes: usize,
        max_total_bytes: usize,
    ) -> (Self, bool) {
        let mut bounded = Self {
            next_inner: self.next_inner,
            next_external: self.next_external,
            order: VecDeque::new(),
            by_inner: BTreeMap::new(),
            by_uri: BTreeMap::new(),
        };
        let mut retained_bytes = 0_usize;
        let mut complete = true;

        for inner_id in &self.order {
            let Some(entry) = self.by_inner.get(inner_id) else {
                continue;
            };
            let entry_bytes = entry
                .internal_id
                .len()
                .saturating_add(entry.external_id.len())
                .saturating_add(entry.uri.len());
            let next_total = retained_bytes.saturating_add(entry_bytes);
            if entry_bytes > max_entry_bytes || next_total > max_total_bytes {
                complete = false;
                continue;
            }

            let entry = entry.clone();
            if !entry.internal_id.is_empty() {
                bounded.by_uri.insert(
                    (entry.internal_id.clone(), entry.uri.clone()),
                    entry.inner_id,
                );
            }
            bounded.order.push_back(entry.inner_id);
            bounded.by_inner.insert(entry.inner_id, entry);
            retained_bytes = next_total;
        }

        (bounded, complete)
    }

    pub(crate) fn entry_fits(&self, inner_id: u32, max_bytes: usize) -> bool {
        self.get(inner_id).is_none_or(|entry| {
            entry
                .internal_id
                .len()
                .saturating_add(entry.external_id.len())
                .saturating_add(entry.uri.len())
                <= max_bytes
        })
    }

    fn enforce_limit(&mut self) {
        while self.order.len() > MAX_HYPERLINKS {
            let Some(oldest_inner) = self.order.pop_front() else {
                break;
            };
            let Some(entry) = self.by_inner.remove(&oldest_inner) else {
                continue;
            };
            if !entry.internal_id.is_empty() {
                self.by_uri.remove(&(entry.internal_id, entry.uri));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Hyperlinks, MAX_HYPERLINKS};

    #[test]
    fn deduplicates_non_anonymous_links() {
        let mut hyperlinks = Hyperlinks::new();

        let left = hyperlinks.put("https://example.com", Some("pane"));
        let right = hyperlinks.put("https://example.com", Some("pane"));

        assert_eq!(left, right);
    }

    #[test]
    fn anonymous_links_are_unique() {
        let mut hyperlinks = Hyperlinks::new();

        let left = hyperlinks.put("https://example.com", None);
        let right = hyperlinks.put("https://example.com", None);

        assert_ne!(left, right);
    }

    #[test]
    fn evicts_oldest_entries_when_limit_is_reached() {
        let mut hyperlinks = Hyperlinks::new();

        for index in 0..=MAX_HYPERLINKS {
            let _ = hyperlinks.put(
                &format!("https://example.com/{index}"),
                Some(&format!("id{index}")),
            );
        }

        assert!(hyperlinks.get(1).is_none());
        assert!(hyperlinks
            .get(u32::try_from(MAX_HYPERLINKS + 1).expect("fits in u32"))
            .is_some());
    }
}
