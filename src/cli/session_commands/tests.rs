#[test]
fn attached_new_session_without_dimensions_carries_client_size() {
    use rmux_proto::TerminalSize;
    assert_eq!(
        super::new_session_initial_size(
            None,
            None,
            Some(TerminalSize {
                cols: 136,
                rows: 35
            }),
            "on"
        ),
        Some(TerminalSize {
            cols: 136,
            rows: 34
        }),
    );
}

#[test]
fn new_session_initial_size_preserves_detached_dimensions_and_attached_status() {
    use rmux_proto::TerminalSize;
    let client = Some(TerminalSize {
        cols: 136,
        rows: 35,
    });
    for (cols, rows, expected) in [
        (None, None, None),
        (Some(90), None, Some(TerminalSize { cols: 90, rows: 24 })),
        (None, Some(20), Some(TerminalSize { cols: 80, rows: 20 })),
        (
            Some(90),
            Some(20),
            Some(TerminalSize { cols: 90, rows: 20 }),
        ),
    ] {
        assert_eq!(
            super::new_session_initial_size(cols, rows, None, "on"),
            expected
        );
        for (status, height) in [("on", 34), ("off", 35), ("2", 33)] {
            assert_eq!(
                super::new_session_initial_size(cols, rows, client, status),
                Some(TerminalSize {
                    cols: 136,
                    rows: height
                })
            );
        }
    }
}

#[test]
fn default_client_command_parses_quoted_queues_and_server_aliases() {
    use crate::cli_args::Command;
    let commands = super::default_client_commands_from_value(
        "probe; new-session -d -s 'default-client'",
        vec!["probe=display-message -p 'FIRST SECOND'".into()],
    )
    .unwrap();
    assert!(matches!(&commands[0], Command::DisplayMessage(_)));
    assert!(
        matches!(&commands[1], Command::NewSession(args) if args.detached && args.session_name.as_ref().unwrap().as_str() == "default-client")
    );
    assert!(super::default_client_commands_from_value("", Vec::new())
        .unwrap()
        .is_empty());
    assert!(
        super::default_client_commands_from_value("missing-default-command", Vec::new()).is_err()
    );
}

#[cfg(windows)]
use std::ffi::OsString;

#[cfg(windows)]
use super::{
    internal_client_shell_handoff_from_vars, windows_client_shell_for_parent_name,
    windows_invoking_client_environment, INTERNAL_CLIENT_SHELL_ENV, INTERNAL_TMUX_COMPAT_ENV,
    PUBLIC_BINARY_OVERRIDE_ENV, RMUX_CLIENT_SHELL_ENV,
};
#[cfg(windows)]
use crate::windows_shell::WindowsShellEnvironment;

#[cfg(windows)]
fn env_pair(name: &str, value: &str) -> (OsString, OsString) {
    (OsString::from(name), OsString::from(value))
}

#[cfg(windows)]
#[test]
fn windows_full_cli_uses_trusted_tiny_client_shell_handoff() {
    let vars = [
        env_pair("Path", r"C:\bin"),
        env_pair(PUBLIC_BINARY_OVERRIDE_ENV, r"C:\rmux\rmux.exe"),
        env_pair(INTERNAL_CLIENT_SHELL_ENV, "pwsh.exe"),
    ];

    assert_eq!(
        internal_client_shell_handoff_from_vars(vars).as_deref(),
        Some("pwsh.exe")
    );
}

#[cfg(windows)]
#[test]
fn windows_full_cli_ignores_untrusted_client_shell_handoff() {
    let vars = [
        env_pair("Path", r"C:\bin"),
        env_pair(INTERNAL_CLIENT_SHELL_ENV, "pwsh.exe"),
    ];

    assert_eq!(internal_client_shell_handoff_from_vars(vars), None);
}

#[cfg(windows)]
#[test]
fn windows_full_cli_filters_internal_handoff_environment() {
    let environment = windows_invoking_client_environment(
        [
            env_pair("Path", r"C:\bin"),
            env_pair(RMUX_CLIENT_SHELL_ENV, "stale.exe"),
            env_pair(INTERNAL_CLIENT_SHELL_ENV, "pwsh.exe"),
            env_pair(INTERNAL_TMUX_COMPAT_ENV, "1"),
        ],
        Some("pwsh.exe".to_owned()),
    );

    assert!(environment.iter().any(|entry| entry == r"Path=C:\bin"));
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
fn windows_full_cli_shell_mapping_skips_alias_and_uses_real_pwsh() {
    let root = std::env::temp_dir().join(format!("rmux-full-shell-hint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let alias_dir = root.join("Microsoft").join("WindowsApps");
    let regular_dir = root.join("PowerShell").join("7");
    let powershell = root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let cmd = root.join("System32").join("cmd.exe");
    std::fs::create_dir_all(&alias_dir).expect("WindowsApps fixture directory");
    std::fs::create_dir_all(&regular_dir).expect("regular shell fixture directory");
    std::fs::create_dir_all(powershell.parent().expect("PowerShell fixture parent"))
        .expect("PowerShell fixture directory");
    std::fs::create_dir_all(cmd.parent().expect("cmd fixture parent"))
        .expect("cmd fixture directory");
    std::fs::write(alias_dir.join("pwsh.exe"), b"").expect("alias pwsh fixture");
    std::fs::write(regular_dir.join("pwsh.exe"), b"").expect("regular pwsh fixture");
    std::fs::write(&powershell, b"").expect("Windows PowerShell fixture");
    std::fs::write(&cmd, b"").expect("cmd fixture");

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

    let alias_path = std::env::join_paths([alias_dir.as_os_str()]).expect("alias-only PATH");
    let alias_environment = WindowsShellEnvironment::for_test(
        Some(alias_path),
        Some(root.clone().into_os_string()),
        Some(cmd.into_os_string()),
    );
    assert_eq!(
        windows_client_shell_for_parent_name("pwsh.exe", &alias_environment).as_deref(),
        Some("powershell.exe")
    );

    std::fs::remove_dir_all(root).expect("remove full shell fixture");
}

#[test]
fn new_session_group_target_reads_the_stored_backslash_name() {
    // `new -s 'b\c'` stores `b\\c`; tmux 3.7b groups with it from `-t 'b\\c'`.
    let cli = crate::cli_args::parse(["rmux", "new-session", "-d", "-t", "b\\\\c"]).unwrap();
    let Some(crate::cli_args::Command::NewSession(args)) = cli.command else {
        panic!("new-session");
    };
    assert_eq!(args.group_target.unwrap().as_str(), "b\\\\c");
}
