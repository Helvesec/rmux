//! Frozen compiler/SemVer canary, independent of the current product API.

pub fn retained(value: u32) -> u32 {
    value
}

#[cfg(all(feature = "web", feature = "regex", not(feature = "crossterm")))]
pub fn combination_only() -> u32 {
    7
}
