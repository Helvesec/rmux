use crate::command_parser::CommandEntry;

#[derive(Clone, Copy)]
struct Metadata {
    entry: CommandEntry,
    extension: bool,
    order: usize,
    usage: &'static str,
}

macro_rules! is_extension {
    (tmux) => {
        false
    };
    (extension) => {
        true
    };
}

macro_rules! metadata {
    (
        commands { $(($name:literal, $alias:expr, $kind:ident, $order:literal,
            $usage:literal, $variant:ident($($args:ty)?), $help:ty, [$($policy:tt)*]);)* }
        delegated { $(($raw:literal, $raw_alias:expr, $raw_kind:ident, $raw_order:literal,
            $raw_usage:literal, $delegate:ident);)* }
        local { $(($local:literal, $local_args:ty, $local_usage:literal);)* }
        aliases { $(($expansion_alias:literal, $expansion:literal, $expansion_args:ty);)* }
    ) => {
        const METADATA: &[Metadata] = &[
            $(Metadata { entry: CommandEntry { name: $name, alias: $alias },
                extension: is_extension!($kind), order: $order, usage: $usage },)*
            $(Metadata { entry: CommandEntry { name: $raw, alias: $raw_alias },
                extension: is_extension!($raw_kind), order: $raw_order, usage: $raw_usage },)*
        ];
        pub(crate) const EXPANSION_ALIASES: &[(&str, &str)] = &[
            $(($expansion_alias, $expansion),)*
        ];
    };
}
crate::command_schema!(metadata);

const fn count(extension: bool) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < METADATA.len() {
        if METADATA[index].extension == extension {
            count += 1;
        }
        index += 1;
    }
    count
}

const fn entries<const N: usize>(extension: bool) -> [CommandEntry; N] {
    let mut result = [CommandEntry {
        name: "",
        alias: None,
    }; N];
    let mut index = 0;
    while index < METADATA.len() {
        let metadata = METADATA[index];
        if metadata.extension == extension {
            result[metadata.order] = metadata.entry;
        }
        index += 1;
    }
    result
}

const fn signatures<const N: usize>() -> [(&'static str, &'static str); N] {
    let mut result = [("", ""); N];
    let mut index = 0;
    while index < METADATA.len() {
        let metadata = METADATA[index];
        let offset = if metadata.extension { count(false) } else { 0 };
        result[offset + metadata.order] = (metadata.entry.name, metadata.usage);
        index += 1;
    }
    result
}

pub(crate) const TMUX_COMMANDS: &[CommandEntry] = &entries::<{ count(false) }>(false);
pub(crate) const EXTENSION_COMMANDS: &[CommandEntry] = &entries::<{ count(true) }>(true);
pub(crate) const SIGNATURES: &[(&str, &str)] = &signatures::<{ METADATA.len() }>();

/// Keep the ledger's literal projection bound to the actual typed declaration.
pub(crate) const fn verify_signatures(projected: &[(&str, &str)]) {
    assert!(
        projected.len() == SIGNATURES.len(),
        "stale command signature projection"
    );
    let mut index = 0;
    while index < SIGNATURES.len() {
        assert!(
            same_text(projected[index].0, SIGNATURES[index].0),
            "stale command name projection"
        );
        assert!(
            same_text(projected[index].1, SIGNATURES[index].1),
            "stale command usage projection"
        );
        index += 1;
    }
}

const fn same_text(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}
