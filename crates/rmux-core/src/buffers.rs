//! Server-wide paste buffer store with deterministic naming and FIFO eviction.

use std::collections::BTreeMap;

use chrono::Local;
use rmux_proto::RmuxError;

use crate::vis::encode_buffer_sample;

/// A single paste buffer entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BufferEntry {
    /// The buffer content.
    content: Vec<u8>,
    /// Monotonic insertion/replacement order for stack-head tracking.
    order: u64,
    /// Creation timestamp matching tmux's `buffer_created`.
    created: i64,
    /// Whether this buffer was auto-named (`buffer%d`).
    unnamed: bool,
}

/// Server-wide buffer store.
///
/// Buffers are server-global state, not session-keyed. Unnamed buffers use
/// deterministic names (`buffer0`, `buffer1`, …) from a monotonically
/// increasing sequence. The allocator skips names already occupied by existing
/// buffers, including explicit named buffers. FIFO eviction at the effective
/// `buffer-limit` removes only unnamed buffers.
#[derive(Debug, Clone, Default)]
pub struct BufferStore {
    /// Buffers keyed by name.
    buffers: BTreeMap<String, BufferEntry>,
    /// Monotonic counter for unnamed buffer names.
    next_unnamed_id: u32,
    /// Monotonic counter for insertion/replacement ordering (stack head).
    next_order: u64,
}

impl BufferStore {
    /// Creates an empty buffer store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of buffers currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    /// Returns whether the store contains no buffers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    /// Returns the name of the stack-head buffer (most recently created/replaced).
    #[must_use]
    pub fn stack_head(&self) -> Option<&str> {
        self.buffers
            .iter()
            .max_by_key(|(_, entry)| entry.order)
            .map(|(name, _)| name.as_str())
    }

    /// Returns the name of the most recent automatic buffer.
    #[must_use]
    pub fn top_unnamed(&self) -> Option<&str> {
        self.buffers
            .iter()
            .filter(|(_, entry)| entry.unnamed)
            .max_by_key(|(_, entry)| entry.order)
            .map(|(name, _)| name.as_str())
    }

    /// Returns the content of the named buffer, or `None` if it does not exist.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.buffers.get(name).map(|entry| entry.content.as_slice())
    }

    /// Sets a named buffer, replacing it if it already exists.
    ///
    /// When `name` is `Some`, the buffer is created or replaced with that name.
    /// When `name` is `None`, a new unnamed buffer is created with the next
    /// deterministic name.
    ///
    /// Zero-length content succeeds without creating or replacing any buffer.
    ///
    /// Returns a list of buffer names evicted by FIFO unnamed eviction (may be
    /// empty), followed by the optional name of the created/replaced buffer.
    pub fn set(
        &mut self,
        name: Option<&str>,
        content: Vec<u8>,
        buffer_limit: usize,
    ) -> Result<SetBufferOutcome, RmuxError> {
        if let Some(name) = name {
            validate_buffer_name(name)?;
        }
        if content.is_empty() {
            return Ok(SetBufferOutcome {
                buffer_name: None,
                evicted: Vec::new(),
            });
        }

        let order = self.next_order;
        self.next_order += 1;
        let created = Local::now().timestamp();

        let (buffer_name, is_new_unnamed) = match name {
            Some(name) => {
                self.buffers.insert(
                    name.to_owned(),
                    BufferEntry {
                        content,
                        order,
                        created,
                        unnamed: false,
                    },
                );
                (name.to_owned(), false)
            }
            None => {
                let buffer_name = self.allocate_unnamed_name()?;
                self.buffers.insert(
                    buffer_name.clone(),
                    BufferEntry {
                        content,
                        order,
                        created,
                        unnamed: true,
                    },
                );
                (buffer_name, true)
            }
        };

        let mut evicted = Vec::new();
        if is_new_unnamed && buffer_limit > 0 {
            let unnamed_count = self.buffers.values().filter(|entry| entry.unnamed).count();
            if unnamed_count > buffer_limit {
                let to_evict = unnamed_count - buffer_limit;
                let mut unnamed_by_order: Vec<(String, u64)> = self
                    .buffers
                    .iter()
                    .filter(|(_, entry)| entry.unnamed)
                    .map(|(name, entry)| (name.clone(), entry.order))
                    .collect();
                unnamed_by_order.sort_by_key(|(_, order)| *order);

                for (name, _) in unnamed_by_order.into_iter().take(to_evict) {
                    self.buffers.remove(&name);
                    evicted.push(name);
                }
            }
        }

        Ok(SetBufferOutcome {
            buffer_name: Some(buffer_name),
            evicted,
        })
    }

    /// Renames a buffer, replacing any existing destination buffer.
    pub fn rename(
        &mut self,
        old_name: Option<&str>,
        new_name: &str,
    ) -> Result<RenameBufferOutcome, RmuxError> {
        validate_buffer_name(new_name)?;

        let old_name = match old_name {
            Some(name) => {
                if !self.buffers.contains_key(name) {
                    return Err(RmuxError::Server(format!("no buffer {name}")));
                }
                name.to_owned()
            }
            None => self
                .top_unnamed()
                .map(str::to_owned)
                .ok_or_else(|| RmuxError::Server("no buffer".to_owned()))?,
        };

        if old_name == new_name {
            return Ok(RenameBufferOutcome {
                old_name,
                new_name: new_name.to_owned(),
                replaced: false,
                changed: false,
            });
        }

        let replaced = self.buffers.remove(new_name).is_some();
        let mut entry = self
            .buffers
            .remove(&old_name)
            .expect("rename source existence was prevalidated");
        entry.unnamed = false;
        self.buffers.insert(new_name.to_owned(), entry);

        Ok(RenameBufferOutcome {
            old_name,
            new_name: new_name.to_owned(),
            replaced,
            changed: true,
        })
    }

    /// Deletes a buffer by name.
    ///
    /// When `name` is `None`, deletes the most recent automatic buffer.
    /// Returns the name of the deleted buffer.
    pub fn delete(&mut self, name: Option<&str>) -> Result<String, RmuxError> {
        let target = match name {
            Some(name) => name.to_owned(),
            None => self
                .top_unnamed()
                .map(str::to_owned)
                .ok_or_else(|| RmuxError::Server("no buffer".to_owned()))?,
        };

        if self.buffers.remove(&target).is_none() {
            return Err(RmuxError::Server(format!("no buffer {target}")));
        }

        Ok(target)
    }

    /// Deletes a buffer only when the current entry still matches `order`.
    ///
    /// Returns `true` when the matching entry was removed. Returns `false`
    /// when the buffer is absent or has since been replaced.
    pub fn delete_if_order_matches(&mut self, name: &str, order: u64) -> bool {
        if self
            .buffers
            .get(name)
            .is_some_and(|entry| entry.order == order)
        {
            self.buffers.remove(name);
            true
        } else {
            false
        }
    }

    /// Returns the content of a buffer by name, or the most recent automatic
    /// buffer when `name` is `None`.
    pub fn show(&self, name: Option<&str>) -> Result<(&str, &[u8]), RmuxError> {
        let (name, content, _) = self.show_with_order(name)?;
        Ok((name, content))
    }

    /// Returns the content and current replacement order for a buffer.
    pub fn show_with_order(&self, name: Option<&str>) -> Result<(&str, &[u8], u64), RmuxError> {
        let resolved = match name {
            Some(name) => {
                if !self.buffers.contains_key(name) {
                    return Err(RmuxError::Server(format!("no buffer {name}")));
                }
                name.to_owned()
            }
            None => self
                .top_unnamed()
                .ok_or_else(|| RmuxError::Server("no buffers".to_owned()))?
                .to_owned(),
        };

        let (key, entry) = self
            .buffers
            .get_key_value(&resolved)
            .expect("buffer existence was verified above");
        Ok((key.as_str(), &entry.content, entry.order))
    }

    /// Returns immutable buffer entries for server-side formatting and sorting.
    #[must_use]
    pub fn entries(&self) -> Vec<BufferView<'_>> {
        self.buffers
            .iter()
            .map(|(name, entry)| BufferView {
                name,
                content: &entry.content,
                order: entry.order,
                created: entry.created,
            })
            .collect()
    }

    /// Returns a formatted listing of all buffers, ordered by most recent first.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        let mut entries = self.entries();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.order));
        entries
            .into_iter()
            .map(|entry| entry.default_line())
            .collect()
    }

    fn allocate_unnamed_name(&mut self) -> Result<String, RmuxError> {
        loop {
            let id = self.next_unnamed_id;
            self.next_unnamed_id = self
                .next_unnamed_id
                .checked_add(1)
                .ok_or_else(|| RmuxError::Server("unnamed buffer sequence exhausted".to_owned()))?;
            let name = format!("buffer{id}");
            if !self.buffers.contains_key(&name) {
                return Ok(name);
            }
        }
    }
}

/// The outcome of a `set` operation on the buffer store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetBufferOutcome {
    /// The name of the created or replaced buffer when one was stored.
    buffer_name: Option<String>,
    /// Names of unnamed buffers evicted by FIFO eviction (oldest first).
    evicted: Vec<String>,
}

impl SetBufferOutcome {
    /// Returns the name of the created or replaced buffer.
    #[must_use]
    pub fn buffer_name(&self) -> Option<&str> {
        self.buffer_name.as_deref()
    }

    /// Returns the names of evicted unnamed buffers.
    #[must_use]
    pub fn evicted(&self) -> &[String] {
        &self.evicted
    }
}

/// Outcome details for a rename operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameBufferOutcome {
    old_name: String,
    new_name: String,
    replaced: bool,
    changed: bool,
}

impl RenameBufferOutcome {
    /// Returns the previous buffer name.
    #[must_use]
    pub fn old_name(&self) -> &str {
        &self.old_name
    }

    /// Returns the resulting buffer name.
    #[must_use]
    pub fn new_name(&self) -> &str {
        &self.new_name
    }

    /// Returns whether an existing destination buffer was replaced.
    #[must_use]
    pub const fn replaced(&self) -> bool {
        self.replaced
    }

    /// Returns whether the rename changed the store.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }
}

/// One borrow-only buffer view for list and format rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferView<'a> {
    name: &'a str,
    content: &'a [u8],
    order: u64,
    created: i64,
}

impl<'a> BufferView<'a> {
    /// Returns the tmux-visible buffer name.
    #[must_use]
    pub fn name(&self) -> &'a str {
        self.name
    }

    /// Returns the raw binary buffer content.
    #[must_use]
    pub fn content(&self) -> &'a [u8] {
        self.content
    }

    /// Returns the monotonic stack-order value.
    #[must_use]
    pub const fn order(&self) -> u64 {
        self.order
    }

    /// Returns the tmux-compatible created timestamp.
    #[must_use]
    pub const fn created(&self) -> i64 {
        self.created
    }

    /// Returns the buffer size in bytes.
    #[must_use]
    pub fn size(&self) -> usize {
        self.content.len()
    }

    /// Returns the tmux-compatible sample preview.
    #[must_use]
    pub fn sample(&self) -> String {
        buffer_preview(self.content)
    }

    /// Returns the default `list-buffers` line for this entry.
    #[must_use]
    pub fn default_line(&self) -> String {
        format!(
            "{}: {} bytes: \"{}\"",
            self.name(),
            self.size(),
            self.sample()
        )
    }
}

/// Validates a buffer name: tmux only rejects empty names.
fn validate_buffer_name(name: &str) -> Result<(), RmuxError> {
    if name.is_empty() {
        return Err(RmuxError::Server(
            "buffer name must not be empty".to_owned(),
        ));
    }
    Ok(())
}

/// Returns a short preview of buffer content for `list-buffers` output.
fn buffer_preview(content: &[u8]) -> String {
    encode_buffer_sample(content)
}

#[cfg(test)]
#[path = "buffers/tests.rs"]
mod tests;
