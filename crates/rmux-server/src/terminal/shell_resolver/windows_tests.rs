use super::*;
use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

#[test]
fn search_path_skips_windowsapps_alias_candidates() {
    let root = unique_test_dir("windowsapps-alias");
    let windows_apps = root.join("Microsoft").join("WindowsApps");
    let regular_bin = root.join("regular-bin");
    fs::create_dir_all(&windows_apps).expect("windowsapps test directory");
    fs::create_dir_all(&regular_bin).expect("regular test directory");
    fs::write(windows_apps.join("pwsh.exe"), b"").expect("windowsapps pwsh fixture");
    fs::write(regular_bin.join("pwsh.exe"), b"").expect("regular pwsh fixture");
    let path =
        env::join_paths([windows_apps.as_os_str(), regular_bin.as_os_str()]).expect("joined PATH");

    let resolved = search_path_in(
        Path::new("pwsh.exe"),
        path.as_os_str(),
        Some(OsStr::new(".EXE")),
    )
    .expect("regular pwsh should resolve");

    assert_eq!(resolved, regular_bin.join("pwsh.exe"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn search_path_rejects_only_windowsapps_alias_candidates() {
    let root = unique_test_dir("only-windowsapps");
    let windows_apps = root.join("Microsoft").join("WindowsApps");
    fs::create_dir_all(&windows_apps).expect("windowsapps test directory");
    fs::write(windows_apps.join("pwsh.exe"), b"").expect("windowsapps pwsh fixture");
    let path = env::join_paths([windows_apps.as_os_str()]).expect("joined PATH");

    let resolved = search_path_in(
        Path::new("pwsh.exe"),
        path.as_os_str(),
        Some(OsStr::new(".EXE")),
    );

    assert_eq!(resolved, None);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn search_path_allows_packaged_windowsapps_executables() {
    let root = unique_test_dir("packaged-windowsapps");
    let packaged = root
        .join("Program Files")
        .join("WindowsApps")
        .join("Microsoft.PowerShell_7.6.1.0_x64__8wekyb3d8bbwe");
    fs::create_dir_all(&packaged).expect("packaged windowsapps test directory");
    fs::write(packaged.join("pwsh.exe"), b"").expect("packaged pwsh fixture");
    let path = env::join_paths([packaged.as_os_str()]).expect("joined PATH");

    let resolved = search_path_in(
        Path::new("pwsh.exe"),
        path.as_os_str(),
        Some(OsStr::new(".EXE")),
    )
    .expect("packaged pwsh should resolve");

    assert_eq!(resolved, packaged.join("pwsh.exe"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn resolve_program_path_uses_effective_environment_path_case_insensitively() {
    let root = unique_test_dir("effective-path");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("bin test directory");
    fs::write(bin.join("rmux-probe.exe"), b"").expect("probe fixture");
    let path = env::join_paths([bin.as_os_str()]).expect("joined PATH");
    let environment = HashMap::from([
        ("Path".to_owned(), path.to_string_lossy().into_owned()),
        ("PATHEXT".to_owned(), ".EXE".to_owned()),
    ]);

    let resolved = resolve_program_path(Path::new("rmux-probe"), &environment);

    assert_eq!(resolved, bin.join("rmux-probe.EXE"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn resolve_program_path_uses_child_cwd_for_relative_path_entries() {
    let root = unique_test_dir("relative-effective-path");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("bin test directory");
    fs::write(bin.join("rmux-probe.exe"), b"").expect("probe fixture");
    let environment = HashMap::from([
        ("PATH".to_owned(), "bin".to_owned()),
        ("PATHEXT".to_owned(), ".EXE".to_owned()),
    ]);

    let resolved = resolve_program_path_from(Path::new("rmux-probe"), &environment, &root);

    assert_eq!(resolved, bin.join("rmux-probe.EXE"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn resolve_program_path_completes_explicit_paths_from_child_cwd_and_pathext() {
    let root = unique_test_dir("explicit-pathext");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("bin test directory");
    let absolute = bin.join("absolute-shim.CMD");
    let relative = bin.join("relative-shim.BAT");
    fs::write(&absolute, b"").expect("absolute batch fixture");
    fs::write(&relative, b"").expect("relative batch fixture");
    let environment = HashMap::from([("PATHEXT".to_owned(), "CMD;.BAT".to_owned())]);

    let resolved_absolute = resolve_program_path_from(
        &bin.join("absolute-shim"),
        &environment,
        Path::new(r"C:\ignored-for-absolute-path"),
    );
    let resolved_relative = resolve_program_path_from(
        &PathBuf::from("bin").join("relative-shim"),
        &environment,
        &root,
    );

    assert_eq!(resolved_absolute, absolute);
    assert_eq!(resolved_relative, relative);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn explicit_default_shell_uses_effective_environment_path() {
    let root = unique_test_dir("default-shell-path");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).expect("bin test directory");
    fs::write(bin.join("custom-shell.exe"), b"").expect("custom shell fixture");
    let path = env::join_paths([bin.as_os_str()]).expect("joined PATH");
    let environment = HashMap::from([
        ("PATH".to_owned(), path.to_string_lossy().into_owned()),
        ("PATHEXT".to_owned(), ".EXE".to_owned()),
    ]);
    let mut options = OptionStore::new();
    options
        .set(
            rmux_proto::ScopeSelector::Global,
            OptionName::DefaultShell,
            "custom-shell".to_owned(),
            rmux_proto::SetOptionMode::Replace,
        )
        .expect("default-shell set succeeds");

    let resolved = resolve_shell_path(&options, None, &environment);

    assert_eq!(resolved, bin.join("custom-shell.EXE"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn explicit_default_shell_normalizes_forward_slashes_without_mutating_option() {
    let root = unique_test_dir("default-shell-forward-slashes");
    fs::create_dir_all(&root).expect("shell test directory");
    let shell = root.join("custom-shell.exe");
    fs::write(&shell, b"").expect("custom shell fixture");
    let configured = shell.to_string_lossy().replace('\\', "/");
    let mut options = OptionStore::new();
    options
        .set(
            rmux_proto::ScopeSelector::Global,
            OptionName::DefaultShell,
            configured.clone(),
            rmux_proto::SetOptionMode::Replace,
        )
        .expect("default-shell set succeeds");

    let resolved = resolve_shell_path(&options, None, &HashMap::new());

    assert_eq!(
        resolved.as_os_str().encode_wide().collect::<Vec<_>>(),
        shell.as_os_str().encode_wide().collect::<Vec<_>>()
    );
    assert_eq!(
        options.global_value(OptionName::DefaultShell),
        Some(configured.as_str())
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn inherited_client_shell_normalizes_forward_slashes_for_execution() {
    let root = unique_test_dir("client-shell-forward-slashes");
    fs::create_dir_all(&root).expect("shell test directory");
    let shell = root.join("custom-shell.exe");
    fs::write(&shell, b"").expect("custom shell fixture");
    let configured = shell.to_string_lossy().replace('\\', "/");
    let environment = HashMap::from([(CLIENT_SHELL_ENV.to_owned(), configured)]);

    let resolved = resolve_shell_path(&OptionStore::new(), None, &environment);

    assert_eq!(
        resolved.as_os_str().encode_wide().collect::<Vec<_>>(),
        shell.as_os_str().encode_wide().collect::<Vec<_>>()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn fallback_cmd_shell_normalizes_forward_slashes_for_execution() {
    let root = unique_test_dir("fallback-shell-forward-slashes");
    fs::create_dir_all(&root).expect("shell test directory");
    let shell = root.join("cmd.exe");
    fs::write(&shell, b"").expect("cmd shell fixture");
    let configured = shell.to_string_lossy().replace('\\', "/");
    let environment = HashMap::from([("COMSPEC".to_owned(), configured)]);

    let resolved = resolve_shell_path(&OptionStore::new(), None, &environment);

    assert_eq!(
        resolved.as_os_str().encode_wide().collect::<Vec<_>>(),
        shell.as_os_str().encode_wide().collect::<Vec<_>>()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn shell_execution_path_normalization_is_lossless_for_non_unicode_units() {
    let path = PathBuf::from(OsString::from_wide(&[
        b'C' as u16,
        b':' as u16,
        b'/' as u16,
        0xD800,
        b'/' as u16,
        b'x' as u16,
    ]));

    let resolved = normalize_windows_shell_execution_path(path);

    assert_eq!(
        resolved.as_os_str().encode_wide().collect::<Vec<_>>(),
        vec![
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xD800,
            b'\\' as u16,
            b'x' as u16,
        ]
    );
}

#[test]
fn shell_execution_path_normalization_handles_unc_without_network_access() {
    let path = PathBuf::from("//server/share/shells/cmd.exe");

    let resolved = normalize_windows_shell_execution_path(path);

    assert_eq!(resolved, PathBuf::from(r"\\server\share\shells\cmd.exe"));
}

#[test]
fn inherited_client_shell_hint_is_used_when_default_shell_is_unset() {
    let environment = HashMap::from([(CLIENT_SHELL_ENV.to_owned(), "cmd.exe".to_owned())]);
    let options = OptionStore::new();

    let resolved = resolve_shell_path(&options, None, &environment);
    let leaf = resolved
        .file_name()
        .expect("resolved shell has a leaf")
        .to_string_lossy()
        .to_ascii_lowercase();

    assert_eq!(leaf, "cmd.exe");
}

#[test]
fn alias_only_client_shell_hint_falls_back_instead_of_retaining_bare_name() {
    let root = unique_test_dir("client-shell-alias");
    let windows_apps = root.join("Microsoft").join("WindowsApps");
    let cmd = root.join("cmd.exe");
    fs::create_dir_all(&windows_apps).expect("WindowsApps test directory");
    fs::write(windows_apps.join("pwsh.exe"), b"").expect("alias pwsh fixture");
    fs::write(&cmd, b"").expect("cmd fixture");
    let path = env::join_paths([windows_apps.as_os_str()]).expect("alias-only PATH");
    let environment = HashMap::from([
        (CLIENT_SHELL_ENV.to_owned(), "pwsh.exe".to_owned()),
        ("PATH".to_owned(), path.to_string_lossy().into_owned()),
        ("PATHEXT".to_owned(), ".EXE".to_owned()),
        ("COMSPEC".to_owned(), cmd.to_string_lossy().into_owned()),
    ]);

    let resolved = resolve_shell_path(&OptionStore::new(), None, &environment);

    assert_eq!(resolved, cmd);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn explicit_default_shell_overrides_inherited_client_shell_hint() {
    let environment = HashMap::from([(CLIENT_SHELL_ENV.to_owned(), "powershell.exe".to_owned())]);
    let mut options = OptionStore::new();
    options
        .set(
            rmux_proto::ScopeSelector::Global,
            OptionName::DefaultShell,
            "cmd.exe".to_owned(),
            rmux_proto::SetOptionMode::Replace,
        )
        .expect("default-shell set succeeds");

    let resolved = resolve_shell_path(&options, None, &environment);
    let leaf = resolved
        .file_name()
        .expect("resolved shell has a leaf")
        .to_string_lossy()
        .to_ascii_lowercase();

    assert_eq!(leaf, "cmd.exe");
}

#[test]
fn invalid_explicit_default_shell_falls_back_to_inherited_client_shell() {
    let environment = HashMap::from([(CLIENT_SHELL_ENV.to_owned(), "cmd.exe".to_owned())]);
    let mut options = OptionStore::new();
    options
        .set(
            rmux_proto::ScopeSelector::Global,
            OptionName::DefaultShell,
            r"C:\definitely\missing\rmux-shell.exe".to_owned(),
            rmux_proto::SetOptionMode::Replace,
        )
        .expect("default-shell set succeeds");

    let resolved = resolve_shell_path(&options, None, &environment);

    assert_eq!(
        resolved
            .file_name()
            .expect("fallback shell has a leaf")
            .to_string_lossy()
            .to_ascii_lowercase(),
        "cmd.exe"
    );
}

fn unique_test_dir(label: &str) -> PathBuf {
    let path = env::temp_dir().join(format!(
        "rmux-shell-resolver-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    path
}
