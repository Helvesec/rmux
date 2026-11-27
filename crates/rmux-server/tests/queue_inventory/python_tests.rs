use super::*;

#[test]
fn windows_alias_filter_keeps_packaged_installations() {
    let root = std::env::temp_dir();
    assert!(is_app_alias(
        &root.join("mIcRoSoFt/wInDoWsApPs/python3.exe")
    ));
    assert!(!is_app_alias(
        &root.join("Program Files/WindowsApps/Python/python.exe")
    ));
}

#[test]
fn windows_discovery_skips_store_aliases_and_tries_later_same_name() {
    let root = std::env::temp_dir();
    let aliases = root.join("Microsoft").join("WindowsApps");
    let denied = root.join("denied");
    let installed = root.join("installed");
    let expected = installed.join("python3.exe");
    let mut attempts = Vec::new();
    let selected = select_windows(None, &[aliases, denied.clone(), installed], |path, args| {
        attempts.push(path.to_owned());
        assert!(args.is_empty());
        if path == expected {
            Ok(expected.clone())
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).to_string())
        }
    })
    .unwrap();
    assert_eq!(selected, expected);
    assert_eq!(attempts, [denied.join("python3.exe"), expected]);
}

#[test]
fn windows_discovery_accepts_python_and_pins_launcher_interpreter() {
    let root = std::env::temp_dir().join("installed");
    for name in ["python.exe", "py.exe"] {
        let interpreter = root.join("runtime").join("python.exe");
        let selected = select_windows(None, std::slice::from_ref(&root), |path, args| {
            if path == root.join(name) {
                assert_eq!(args, if name == "py.exe" { &["-3"][..] } else { &[] });
                Ok(interpreter.clone())
            } else {
                Err("not Python 3".into())
            }
        })
        .unwrap();
        assert_eq!(selected, interpreter);
    }
}

#[test]
fn windows_explicit_python_is_authoritative_even_when_broken() {
    let root = std::env::temp_dir();
    let explicit = root.join("chosen python.exe");
    for working in [true, false] {
        let mut attempts = 0;
        let result = select_windows(
            Some(explicit.as_os_str()),
            std::slice::from_ref(&root),
            |path, args| {
                attempts += 1;
                assert_eq!(path, explicit);
                assert!(args.is_empty());
                if working {
                    Ok(explicit.clone())
                } else {
                    Err("Access Denied".into())
                }
            },
        );
        assert_eq!(attempts, 1);
        if working {
            assert_eq!(result.unwrap(), explicit);
        } else {
            let error = result.unwrap_err();
            assert!(error.contains("RMUX_PYTHON"));
            assert!(error.contains("Access Denied"));
        }
    }
}

#[test]
fn windows_missing_python_reports_attempts_instead_of_skipping_inventory() {
    let root = std::env::temp_dir();
    let error = select_windows(None, &[root], |_, _| Err("unusable".into())).unwrap_err();
    for name in [
        "RMUX_PYTHON",
        "python3.exe",
        "python.exe",
        "py.exe",
        "unusable",
    ] {
        assert!(error.contains(name), "{error}");
    }
}

#[cfg(unix)]
#[test]
fn windows_policy_recovers_from_real_permission_denied_and_pins_python() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let selected = std::env::var_os("RMUX_PYTHON").unwrap_or_else(|| "python3".into());
    let python = probe(Path::new(&selected), &[]).unwrap();
    let root = std::env::temp_dir().join(format!("rmux-python-discovery-{}", std::process::id()));
    let denied = root.join("denied");
    let installed = root.join("installed");
    std::fs::create_dir_all(&denied).unwrap();
    std::fs::create_dir(&installed).unwrap();
    let alias = denied.join("python3.exe");
    std::fs::write(&alias, b"inert alias\n").unwrap();
    std::fs::set_permissions(&alias, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&python, installed.join("python3.exe")).unwrap();

    // The old single-candidate launch fails before any Python code runs.
    assert_eq!(
        Command::new(&alias)
            .arg("--version")
            .output()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let resolved = select_windows(None, &[denied, installed], probe).unwrap();
    let output = Command::new(resolved)
        .env("PATH", &root)
        .args(["-c", "import sys; assert sys.version_info.major == 3"])
        .output()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(output.status.success());
}

#[test]
fn probe_rejects_a_non_interpreter() {
    // The test harness exits successfully for --list but is not a Python probe.
    let error = probe(&std::env::current_exe().unwrap(), &["--list"]).unwrap_err();
    assert!(!error.is_empty());
}
