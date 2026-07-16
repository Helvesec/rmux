//! Windows shell executable classification.

use std::path::Path;

/// Returns whether a shell candidate can be launched directly by RMUX.
///
/// `%LOCALAPPDATA%\Microsoft\WindowsApps` contains app-execution aliases that
/// can exist as files while still being unsuitable as ConPTY applications.
/// Packaged executables below `Program Files\WindowsApps` are not aliases and
/// remain eligible.
#[must_use]
pub fn is_usable_shell_candidate(path: &Path) -> bool {
    let mut previous_was_microsoft = false;
    for component in path.components() {
        let name = component.as_os_str();
        if previous_was_microsoft && name.eq_ignore_ascii_case("WindowsApps") {
            return false;
        }
        previous_was_microsoft = name.eq_ignore_ascii_case("Microsoft");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_user_windowsapps_aliases_only() {
        assert!(!is_usable_shell_candidate(Path::new(
            r"C:\Users\Shadow\AppData\Local\Microsoft\WindowsApps\pwsh.exe"
        )));
        assert!(is_usable_shell_candidate(Path::new(
            r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7_x64__8wekyb3d8bbwe\pwsh.exe"
        )));
        assert!(is_usable_shell_candidate(Path::new(
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        )));
    }
}
