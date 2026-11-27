use super::*;

#[cfg(windows)]
#[test]
fn new_session_tiny_windows_replaces_inherited_client_shell_hint() {
    let environment = windows_invoking_client_environment(
        [
            (OsString::from("Path"), OsString::from("C:\\bin")),
            (
                OsString::from("RMUX_CLIENT_SHELL"),
                OsString::from("stale.exe"),
            ),
            (
                OsString::from("RMUX_INTERNAL_INVOKED_AS_TMUX"),
                OsString::from("1"),
            ),
            (
                OsString::from("RMUX_INTERNAL_CLIENT_SHELL"),
                OsString::from("stale-internal.exe"),
            ),
            (
                OsString::from("USERPROFILE"),
                OsString::from("C:\\Users\\RMUXUser"),
            ),
        ],
        Some("pwsh.exe".to_owned()),
    );

    assert!(environment.iter().any(|entry| entry == "Path=C:\\bin"));
    assert!(environment
        .iter()
        .any(|entry| entry == "USERPROFILE=C:\\Users\\RMUXUser"));
    assert!(environment
        .iter()
        .any(|entry| entry == "RMUX_CLIENT_SHELL=pwsh.exe"));
    assert!(!environment
        .iter()
        .any(|entry| entry == "RMUX_CLIENT_SHELL=stale.exe"));
    assert!(!environment
        .iter()
        .any(|entry| entry.starts_with("RMUX_INTERNAL_CLIENT_SHELL=")));
    assert!(!environment
        .iter()
        .any(|entry| entry.starts_with("RMUX_INTERNAL_INVOKED_AS_TMUX=")));
}

#[cfg(windows)]
#[test]
fn tiny_windows_client_shell_mapping_matches_full_cli_surface() {
    let root = std::env::temp_dir().join(format!("rmux-tiny-shell-hint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let alias_dir = root.join("Microsoft").join("WindowsApps");
    let regular_dir = root.join("PowerShell").join("7");
    let cmd = root.join("System32").join("cmd.exe");
    std::fs::create_dir_all(&alias_dir).expect("WindowsApps fixture directory");
    std::fs::create_dir_all(&regular_dir).expect("regular shell fixture directory");
    std::fs::create_dir_all(cmd.parent().expect("cmd fixture parent"))
        .expect("cmd fixture directory");
    std::fs::write(alias_dir.join("pwsh.exe"), b"").expect("alias pwsh fixture");
    std::fs::write(regular_dir.join("pwsh.exe"), b"").expect("regular pwsh fixture");
    std::fs::write(&cmd, b"").expect("cmd fixture");

    let alias_path = std::env::join_paths([alias_dir.as_os_str()]).expect("alias-only PATH");
    let alias_environment = WindowsShellEnvironment::for_test(
        Some(alias_path),
        Some(root.clone().into_os_string()),
        Some(cmd.clone().into_os_string()),
    );
    let expected_cmd = cmd.to_string_lossy().into_owned();
    assert_eq!(
        windows_client_shell_for_parent_name("pwsh.exe", &alias_environment).as_deref(),
        Some(expected_cmd.as_str())
    );

    let real_path = std::env::join_paths([alias_dir.as_os_str(), regular_dir.as_os_str()])
        .expect("PATH with real pwsh");
    let real_environment = WindowsShellEnvironment::for_test(
        Some(real_path),
        Some(root.clone().into_os_string()),
        Some(cmd.clone().into_os_string()),
    );
    assert_eq!(
        windows_client_shell_for_parent_name("powershell.exe", &real_environment).as_deref(),
        Some("pwsh.exe")
    );
    assert_eq!(
        windows_client_shell_for_parent_name("bash.exe", &real_environment).as_deref(),
        Some("bash.exe")
    );
    assert_eq!(
        windows_client_shell_for_parent_name("unknown.exe", &real_environment),
        None
    );

    std::fs::remove_dir_all(root).expect("remove tiny shell fixture");
}
