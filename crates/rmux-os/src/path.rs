//! Filesystem path helpers shared across OS-boundary crates.

use std::path::Path;

/// Returns the parent directory for `path`, treating an empty relative parent
/// as the current directory.
#[must_use]
pub fn parent_or_current(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        return Some(Path::new("."));
    }
    Some(parent)
}

#[cfg(test)]
mod tests {
    use super::parent_or_current;
    use std::path::Path;

    #[test]
    fn empty_relative_parent_maps_to_current_directory() {
        assert_eq!(
            parent_or_current(Path::new("rmux.sock")),
            Some(Path::new("."))
        );
    }

    #[test]
    fn path_without_parent_returns_none() {
        assert_eq!(parent_or_current(Path::new("")), None);
    }
}
