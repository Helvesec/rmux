//! Filesystem path helpers shared across OS-boundary crates.

use std::path::Path;

#[cfg(not(windows))]
use std::path::PathBuf;

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
/// Unix socket files may be compared before the endpoint exists, so their
/// existing parent directory is canonicalized when the full path is missing.
/// Windows pipe names are opaque kernel object names: compare their UTF-16
/// units with ASCII case folding and never probe the filesystem namespace.
#[must_use]
pub fn socket_paths_match(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        windows_socket_paths_match(left, right)
    }
    #[cfg(not(windows))]
    {
        canonical_socket_path(left) == canonical_socket_path(right)
    }
}

#[cfg(windows)]
fn windows_socket_paths_match(left: &Path, right: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;

    left.as_os_str()
        .encode_wide()
        .map(fold_ascii_utf16_unit)
        .eq(right.as_os_str().encode_wide().map(fold_ascii_utf16_unit))
}

#[cfg(windows)]
fn fold_ascii_utf16_unit(unit: u16) -> u16 {
    if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
        unit + u16::from(b'a' - b'A')
    } else {
        unit
    }
}

#[cfg(not(windows))]
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

    #[cfg(not(windows))]
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

    #[cfg(windows)]
    #[test]
    fn socket_match_does_not_normalize_pipe_name_syntax() {
        assert!(!socket_paths_match(
            Path::new(r"\\.\pipe\rmux\endpoint"),
            Path::new(r"\\.\pipe\rmux\.\endpoint")
        ));
        assert!(!socket_paths_match(
            Path::new(r"\\.\pipe\rmux\endpoint"),
            Path::new(r"\\.\pipe\rmux/endpoint")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn socket_match_only_folds_ascii_case() {
        assert!(!socket_paths_match(
            Path::new(r"\\.\pipe\rmux-Ä"),
            Path::new(r"\\.\pipe\rmux-ä")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn socket_match_preserves_distinct_unpaired_utf16_units() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        use std::path::PathBuf;

        let prefix: Vec<u16> = r"\\.\pipe\rmux-".encode_utf16().collect();
        let mut left = prefix.clone();
        left.push(0xd800);
        let mut right = prefix;
        right.push(0xd801);
        let left = PathBuf::from(OsString::from_wide(&left));
        let right = PathBuf::from(OsString::from_wide(&right));

        assert_eq!(
            left.as_os_str().to_string_lossy(),
            right.as_os_str().to_string_lossy()
        );
        assert!(!socket_paths_match(&left, &right));
    }
}
