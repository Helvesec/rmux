//! Filesystem path helpers shared across OS-boundary crates.

use std::path::{Path, PathBuf};

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

/// Returns whether two socket path spellings address the same local endpoint.
///
/// Socket files and Windows pipe names may be compared before the endpoint
/// exists, so this canonicalizes the whole path when possible and otherwise
/// canonicalizes the existing parent directory only.
#[must_use]
pub fn socket_paths_match(left: &Path, right: &Path) -> bool {
    let left = canonical_socket_path(left);
    let right = canonical_socket_path(right);
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn canonical_socket_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => std::fs::canonicalize(parent)
            .map(|canonical_parent| canonical_parent.join(file_name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::{parent_or_current, socket_paths_match};
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

    #[test]
    fn socket_match_canonicalizes_existing_parent_for_missing_endpoint() {
        let root = std::env::temp_dir().join(format!("rmux-os-socket-path-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("temp root create succeeds");
        let left = root.join("rmux.sock");
        let right = root.join(".").join("rmux.sock");

        assert!(socket_paths_match(&left, &right));

        std::fs::remove_dir_all(root).expect("temp root cleanup succeeds");
    }

    #[cfg(windows)]
    #[test]
    fn socket_match_is_case_insensitive_on_windows() {
        assert!(socket_paths_match(
            Path::new(r"C:\RMUX\socket"),
            Path::new(r"c:\rmux\SOCKET")
        ));
    }
}
