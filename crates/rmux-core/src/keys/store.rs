use std::collections::BTreeMap;

use unicode_width::UnicodeWidthStr;

use crate::command_parser::{
    parse_command_string, CommandArgument, CommandParseError, ParsedCommand, ParsedCommands,
};

use super::{
    defaults, key_string_lookup_key, key_string_lookup_string, strip_flags, KeyCode,
    KEYC_MASK_MODIFIERS,
};

/// Sort orders accepted by `list-keys -O`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyBindingSortOrder {
    /// Sort by key code.
    #[default]
    Key,
    /// Sort by modifier bits.
    Modifier,
    /// Sort by table name.
    Name,
}

impl KeyBindingSortOrder {
    /// Parses a tmux sort-order token.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("key") || value.eq_ignore_ascii_case("index") {
            Some(Self::Key)
        } else if value.eq_ignore_ascii_case("modifier") {
            Some(Self::Modifier)
        } else if value.eq_ignore_ascii_case("name") || value.eq_ignore_ascii_case("title") {
            Some(Self::Name)
        } else {
            None
        }
    }
}

/// One bound key in a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    key: KeyCode,
    note: Option<String>,
    repeat: bool,
    commands: ParsedCommands,
}

impl KeyBinding {
    /// Returns the bound key code.
    #[must_use]
    pub const fn key(&self) -> KeyCode {
        self.key
    }

    /// Returns the optional binding note.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Returns whether the binding repeats.
    #[must_use]
    pub const fn repeat(&self) -> bool {
        self.repeat
    }

    /// Returns the parsed tmux command list.
    #[must_use]
    pub const fn commands(&self) -> &ParsedCommands {
        &self.commands
    }
}

/// One key table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindingTable {
    name: String,
    references: usize,
    active: BTreeMap<KeyCode, KeyBinding>,
    defaults: BTreeMap<KeyCode, KeyBinding>,
}

impl KeyBindingTable {
    /// Returns the table name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the current reference count.
    #[must_use]
    pub const fn references(&self) -> usize {
        self.references
    }

    /// Returns the active binding tree.
    #[must_use]
    pub const fn active(&self) -> &BTreeMap<KeyCode, KeyBinding> {
        &self.active
    }

    /// Returns the default binding tree snapshot.
    #[must_use]
    pub const fn defaults(&self) -> &BTreeMap<KeyCode, KeyBinding> {
        &self.defaults
    }
}

/// A listed key binding paired with derived display fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindingDisplay {
    table_name: String,
    binding: KeyBinding,
    key_string: String,
    command_string: String,
    default_index: Option<usize>,
}

impl KeyBindingDisplay {
    /// Returns the table name.
    #[must_use]
    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    /// Returns the binding.
    #[must_use]
    pub const fn binding(&self) -> &KeyBinding {
        &self.binding
    }

    /// Returns the canonical key string.
    #[must_use]
    pub fn key_string(&self) -> &str {
        &self.key_string
    }

    /// Returns the canonical command string.
    #[must_use]
    pub fn command_string(&self) -> &str {
        &self.command_string
    }
}

/// A mutable table reference operation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindingTableRef {
    name: String,
    created: bool,
}

impl KeyBindingTableRef {
    /// Returns the table name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns whether the table was created.
    #[must_use]
    pub const fn created(&self) -> bool {
        self.created
    }
}

/// Global key table store with tmux-style default snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindingStore {
    tables: BTreeMap<String, KeyBindingTable>,
}

impl Default for KeyBindingStore {
    fn default() -> Self {
        Self::with_defaults().expect("embedded default bindings must parse")
    }
}

impl KeyBindingStore {
    /// Creates an empty key store with no tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tables: BTreeMap::new(),
        }
    }

    /// Creates a store populated with the frozen tmux default bindings.
    pub fn with_defaults() -> Result<Self, CommandParseError> {
        let mut store = Self::new();
        for default in defaults::DEFAULT_BINDING_STRINGS {
            let parsed = parse_command_string(default)?;
            for command in parsed.commands() {
                store.apply_parsed_default(command)?;
            }
        }
        store.snapshot_defaults();
        Ok(store)
    }

    /// Returns the named table, when present.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<&KeyBindingTable> {
        self.tables.get(name)
    }

    /// Iterates every table in name order.
    pub fn tables(&self) -> impl Iterator<Item = &KeyBindingTable> {
        self.tables.values()
    }

    /// Finds or creates a table and increments its reference count.
    pub fn get_table(&mut self, name: &str, create: bool) -> Option<KeyBindingTableRef> {
        if let Some(table) = self.tables.get_mut(name) {
            table.references = table.references.saturating_add(1);
            return Some(KeyBindingTableRef {
                name: name.to_owned(),
                created: false,
            });
        }
        if !create {
            return None;
        }

        self.tables.insert(
            name.to_owned(),
            KeyBindingTable {
                name: name.to_owned(),
                references: 1,
                active: BTreeMap::new(),
                defaults: BTreeMap::new(),
            },
        );
        Some(KeyBindingTableRef {
            name: name.to_owned(),
            created: true,
        })
    }

    /// Drops one reference from a table and removes it if it is fully empty.
    pub fn unref_table(&mut self, name: &str) {
        let should_remove = if let Some(table) = self.tables.get_mut(name) {
            table.references = table.references.saturating_sub(1);
            table.references == 0 && table.active.is_empty() && table.defaults.is_empty()
        } else {
            false
        };
        if should_remove {
            self.tables.remove(name);
        }
    }

    /// Adds or updates a binding in a table.
    pub fn add_binding(
        &mut self,
        table_name: &str,
        key: KeyCode,
        note: Option<String>,
        repeat: bool,
        commands: Option<ParsedCommands>,
    ) -> bool {
        let table = self.ensure_table_mut(table_name);
        let key = strip_flags(key);
        if commands.is_none() {
            if let Some(binding) = table.active.get_mut(&key) {
                if let Some(note) = note {
                    binding.note = Some(note);
                }
                if repeat {
                    binding.repeat = true;
                }
                return true;
            }
            return false;
        }

        table.active.insert(
            key,
            KeyBinding {
                key,
                note,
                repeat,
                commands: commands.expect("checked commands presence"),
            },
        );
        true
    }

    /// Removes one active binding from a table.
    pub fn remove_binding(&mut self, table_name: &str, key: KeyCode) -> bool {
        let key = strip_flags(key);
        let Some(table) = self.tables.get_mut(table_name) else {
            return false;
        };
        let removed = table.active.remove(&key).is_some();
        self.remove_table_if_empty(table_name);
        removed
    }

    /// Restores one active binding from the default snapshot.
    pub fn reset_binding(&mut self, table_name: &str, key: KeyCode) {
        let key = strip_flags(key);
        let Some(table) = self.tables.get_mut(table_name) else {
            return;
        };
        if let Some(default) = table.defaults.get(&key).cloned() {
            table.active.insert(key, default);
        } else {
            table.active.remove(&key);
        }
        self.remove_table_if_empty(table_name);
    }

    /// Removes every binding from a table.
    pub fn remove_table(&mut self, table_name: &str) -> bool {
        let Some(table) = self.tables.get_mut(table_name) else {
            return false;
        };
        let removed = !table.active.is_empty();
        table.active.clear();
        self.remove_table_if_empty(table_name);
        removed
    }

    /// Restores every binding in a table from the default snapshot.
    pub fn reset_table(&mut self, table_name: &str) {
        let Some(table) = self.tables.get_mut(table_name) else {
            return;
        };
        if table.defaults.is_empty() {
            self.remove_table_if_empty(table_name);
            return;
        }
        table.active = table.defaults.clone();
    }

    /// Returns a binding from the active tree.
    #[must_use]
    pub fn get_binding(&self, table_name: &str, key: KeyCode) -> Option<&KeyBinding> {
        self.tables
            .get(table_name)
            .and_then(|table| table.active.get(&strip_flags(key)))
    }

    /// Returns a binding from the default snapshot.
    #[must_use]
    pub fn get_default_binding(&self, table_name: &str, key: KeyCode) -> Option<&KeyBinding> {
        self.tables
            .get(table_name)
            .and_then(|table| table.defaults.get(&strip_flags(key)))
    }

    /// Returns every binding as display rows sorted for `list-keys`.
    #[must_use]
    pub fn list_bindings(
        &self,
        table_name: Option<&str>,
        sort_order: KeyBindingSortOrder,
        reversed: bool,
    ) -> Vec<KeyBindingDisplay> {
        let mut bindings = if let Some(table_name) = table_name {
            self.tables
                .get(table_name)
                .into_iter()
                .flat_map(|table| {
                    table
                        .active
                        .values()
                        .cloned()
                        .map(|binding| display_binding(table, binding))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        } else {
            self.tables
                .values()
                .flat_map(|table| {
                    table
                        .active
                        .values()
                        .cloned()
                        .map(|binding| display_binding(table, binding))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };

        bindings.sort_by(|left, right| {
            let ordering = match sort_order {
                KeyBindingSortOrder::Key => match (left.default_index, right.default_index) {
                    (Some(left), Some(right)) => left.cmp(&right),
                    _ => left.binding.key.cmp(&right.binding.key),
                },
                KeyBindingSortOrder::Modifier => (left.binding.key & KEYC_MASK_MODIFIERS)
                    .cmp(&(right.binding.key & KEYC_MASK_MODIFIERS)),
                KeyBindingSortOrder::Name => left
                    .table_name
                    .to_ascii_lowercase()
                    .cmp(&right.table_name.to_ascii_lowercase()),
            };
            let ordering = if ordering.is_eq() {
                left.table_name
                    .to_ascii_lowercase()
                    .cmp(&right.table_name.to_ascii_lowercase())
                    .then_with(|| left.binding.key.cmp(&right.binding.key))
            } else {
                ordering
            };
            if reversed {
                ordering.reverse()
            } else {
                ordering
            }
        });
        bindings
    }

    /// Returns whether any listed binding repeats.
    #[must_use]
    pub fn has_repeat(bindings: &[KeyBindingDisplay]) -> bool {
        bindings.iter().any(|binding| binding.binding.repeat)
    }

    /// Returns the maximum display width for listed key strings.
    #[must_use]
    pub fn key_string_width(bindings: &[KeyBindingDisplay]) -> usize {
        bindings
            .iter()
            .map(|binding| UnicodeWidthStr::width(binding.key_string.as_str()))
            .max()
            .unwrap_or(0)
    }

    /// Returns the maximum display width for listed table names.
    #[must_use]
    pub fn key_table_width(bindings: &[KeyBindingDisplay]) -> usize {
        bindings
            .iter()
            .map(|binding| UnicodeWidthStr::width(binding.table_name.as_str()))
            .max()
            .unwrap_or(0)
    }

    fn apply_parsed_default(&mut self, command: &ParsedCommand) -> Result<(), CommandParseError> {
        let (table, key, note, repeat, commands) = parse_bind_command(command)?;
        let _ = self.add_binding(&table, key, note, repeat, Some(commands));
        Ok(())
    }

    fn snapshot_defaults(&mut self) {
        for table in self.tables.values_mut() {
            if table.defaults.is_empty() {
                table.defaults = table.active.clone();
            }
        }
    }

    fn ensure_table_mut(&mut self, name: &str) -> &mut KeyBindingTable {
        self.tables
            .entry(name.to_owned())
            .or_insert_with(|| KeyBindingTable {
                name: name.to_owned(),
                references: 0,
                active: BTreeMap::new(),
                defaults: BTreeMap::new(),
            })
    }

    fn remove_table_if_empty(&mut self, table_name: &str) {
        let should_remove = self.tables.get(table_name).is_some_and(|table| {
            table.references == 0 && table.active.is_empty() && table.defaults.is_empty()
        });
        if should_remove {
            self.tables.remove(table_name);
        }
    }
}

fn display_binding(table: &KeyBindingTable, binding: KeyBinding) -> KeyBindingDisplay {
    let default_display = table
        .defaults
        .get(&binding.key)
        .filter(|default| *default == &binding)
        .and_then(|_| defaults::list_keys_display(&table.name, binding.key));
    let key_string = default_display.map_or_else(
        || key_string_lookup_key(binding.key, false),
        |display| display.key_string.to_owned(),
    );
    let command_string = default_display.map_or_else(
        || binding.commands.to_tmux_string(),
        |display| display.command_string.to_owned(),
    );
    KeyBindingDisplay {
        table_name: table.name.clone(),
        binding,
        key_string,
        command_string,
        default_index: default_display.map(|display| display.index),
    }
}

fn parse_bind_command(
    command: &ParsedCommand,
) -> Result<(String, KeyCode, Option<String>, bool, ParsedCommands), CommandParseError> {
    let mut index = 0;
    let mut table_name: Option<String> = None;
    let mut note = None;
    let mut repeat = false;
    let arguments = command.arguments();

    while let Some(argument) = arguments.get(index) {
        let Some(value) = argument.as_string() else {
            break;
        };
        match value {
            "-T" => {
                index += 1;
                table_name = Some(
                    arguments
                        .get(index)
                        .and_then(|argument| argument.as_string())
                        .ok_or_else(|| CommandParseError::new(1, "bind-key missing -T key-table"))?
                        .to_owned(),
                );
            }
            _ if value.starts_with("-T") && value.len() > 2 => {
                table_name = Some(value[2..].to_owned());
            }
            "-n" => table_name = Some("root".to_owned()),
            "-N" => {
                index += 1;
                note = Some(
                    arguments
                        .get(index)
                        .and_then(|argument| argument.as_string())
                        .ok_or_else(|| CommandParseError::new(1, "bind-key missing -N note"))?
                        .to_owned(),
                );
            }
            _ if value.starts_with("-N") && value.len() > 2 => {
                note = Some(value[2..].to_owned());
            }
            "-r" => repeat = true,
            _ if value.starts_with('-') && value.len() > 1 => {
                return Err(CommandParseError::new(
                    1,
                    format!("unsupported default bind-key flag: {value}"),
                ));
            }
            _ => break,
        }
        index += 1;
    }

    let key_string = arguments
        .get(index)
        .and_then(|argument| argument.as_string())
        .ok_or_else(|| CommandParseError::new(1, "bind-key missing key"))?;
    index += 1;
    let key = key_string_lookup_string(key_string)
        .ok_or_else(|| CommandParseError::new(1, format!("unknown key: {key_string}")))?;

    let commands = match arguments.get(index) {
        Some(CommandArgument::Commands(commands)) => commands.clone(),
        Some(CommandArgument::String(command)) => parse_command_string(command)?,
        None => {
            return Err(CommandParseError::new(
                1,
                "default bind-key must include a command list",
            ))
        }
    };

    Ok((
        table_name.unwrap_or_else(|| "prefix".to_owned()),
        key,
        note,
        repeat,
        commands,
    ))
}
