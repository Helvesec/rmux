use super::PromptType;

#[derive(Debug, Default)]
pub(in super::super) struct PromptHistoryStore {
    items: [Vec<String>; 4],
    /// Initial import (or the decision not to import) has completed.
    pub(super) file_initialized: bool,
}

impl PromptHistoryStore {
    pub(super) fn push(&mut self, prompt_type: PromptType, line: &str, limit: usize) {
        let history = &mut self.items[prompt_type.index()];
        if history.last().is_some_and(|existing| existing == line) {
            return;
        }

        if limit == 0 {
            history.clear();
            return;
        }

        history.push(line.to_owned());
        if history.len() > limit {
            let excess = history.len() - limit;
            history.drain(..excess);
        }
    }

    pub(super) fn up(&self, prompt_type: PromptType, index: &mut usize) -> Option<String> {
        let history = &self.items[prompt_type.index()];
        if history.is_empty() || *index == history.len() {
            return None;
        }
        *index += 1;
        history.get(history.len().saturating_sub(*index)).cloned()
    }

    pub(super) fn down(&self, prompt_type: PromptType, index: &mut usize) -> Option<String> {
        let history = &self.items[prompt_type.index()];
        if history.is_empty() || *index == 0 {
            return None;
        }
        *index -= 1;
        if *index == 0 {
            return Some(String::new());
        }
        history.get(history.len().saturating_sub(*index)).cloned()
    }

    /// Renders the history for the selected prompt type (or every type if `None`) using
    /// the tmux `show-prompt-history` output format.
    ///
    /// The exact shape of each section matches tmux's `cmdq_print` calls in
    /// `cmd-show-prompt-history.c`: a `History for <label>:` heading, a blank line,
    /// the `index: entry` rows, and a trailing blank line.
    pub(super) fn render(&self, selected: Option<PromptType>) -> String {
        use std::fmt::Write as _;

        let mut output = String::new();
        let types = selected
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or(PromptType::ALL.as_slice());
        for prompt_type in types {
            let _ = writeln!(output, "History for {}:\n", prompt_type.label());
            for (idx, entry) in self.items[prompt_type.index()].iter().enumerate() {
                let _ = writeln!(output, "{}: {entry}", idx + 1);
            }
            output.push('\n');
        }
        output
    }

    /// Exports every entry, oldest first, in the canonical prompt-type order.
    ///
    /// This is the shape `history-file` persists, so the file reads back in the
    /// same order it was written.
    pub(super) fn export(&self) -> Vec<(PromptType, String)> {
        PromptType::ALL
            .iter()
            .flat_map(|prompt_type| {
                self.items[prompt_type.index()]
                    .iter()
                    .map(move |entry| (*prompt_type, entry.clone()))
            })
            .collect()
    }

    /// Imports entries through `push`, so the current limit and the consecutive
    /// deduplication apply to a loaded file exactly as they do to typing.
    pub(super) fn import(&mut self, entries: &[(PromptType, String)], limit: usize) {
        for (prompt_type, entry) in entries {
            self.push(*prompt_type, entry, limit);
        }
    }

    /// Clears history for the selected prompt type (or every type if `None`).
    pub(super) fn clear(&mut self, selected: Option<PromptType>) {
        match selected {
            Some(prompt_type) => self.items[prompt_type.index()].clear(),
            None => self.items.iter_mut().for_each(Vec::clear),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_up_down_restores_original_buffer() {
        let mut store = PromptHistoryStore::default();
        store.push(PromptType::Command, "old-cmd", 100);

        let mut index = 0usize;
        let up = store.up(PromptType::Command, &mut index);
        assert_eq!(up, Some("old-cmd".to_owned()));
        assert_eq!(index, 1);

        let down = store.down(PromptType::Command, &mut index);
        assert_eq!(down, Some(String::new()));
        assert_eq!(index, 0);
    }

    #[test]
    fn history_push_deduplicates_consecutive() {
        let mut store = PromptHistoryStore::default();
        store.push(PromptType::Command, "abc", 100);
        store.push(PromptType::Command, "abc", 100);
        store.push(PromptType::Command, "abc", 100);

        let mut index = 0;
        assert!(store.up(PromptType::Command, &mut index).is_some());
        assert!(store.up(PromptType::Command, &mut index).is_none());
    }

    #[test]
    fn history_respects_limit() {
        let mut store = PromptHistoryStore::default();
        store.push(PromptType::Command, "a", 2);
        store.push(PromptType::Command, "b", 2);
        store.push(PromptType::Command, "c", 2);

        let mut index = 0;
        let first = store.up(PromptType::Command, &mut index);
        assert_eq!(first, Some("c".to_owned()));
        let second = store.up(PromptType::Command, &mut index);
        assert_eq!(second, Some("b".to_owned()));
        assert!(store.up(PromptType::Command, &mut index).is_none());
    }

    #[test]
    fn history_zero_limit_clears() {
        let mut store = PromptHistoryStore::default();
        store.push(PromptType::Command, "a", 100);
        store.push(PromptType::Command, "b", 0);

        let mut index = 0;
        assert!(store.up(PromptType::Command, &mut index).is_none());
    }

    #[test]
    fn history_types_are_independent() {
        let mut store = PromptHistoryStore::default();
        store.push(PromptType::Command, "cmd", 100);
        store.push(PromptType::Search, "search", 100);

        let mut index = 0;
        let cmd = store.up(PromptType::Command, &mut index);
        assert_eq!(cmd, Some("cmd".to_owned()));

        let mut index = 0;
        let search = store.up(PromptType::Search, &mut index);
        assert_eq!(search, Some("search".to_owned()));
    }
}
