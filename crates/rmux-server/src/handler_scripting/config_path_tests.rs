use super::source_files::{default_config_paths, default_tmux_fallback_paths};
use crate::test_env::EnvVarGuard;

#[cfg(windows)]
use super::source_files::{source_inputs_for_path, SourceReadPolicy};

#[cfg(unix)]
#[test]
fn default_config_paths_use_rmux_locations() {
    let _lock = crate::test_env::lock_blocking();
    let _home = EnvVarGuard::set("HOME", Some("/tmp/rmux-home"));
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some("/tmp/rmux-xdg"));

    let paths = default_config_paths();

    assert_eq!(
        paths,
        vec![
            "/etc/rmux.conf".to_owned(),
            "/tmp/rmux-home/.rmux.conf".to_owned(),
            "/tmp/rmux-xdg/rmux/rmux.conf".to_owned(),
            "/tmp/rmux-home/.config/rmux/rmux.conf".to_owned(),
        ]
    );
    assert!(
        paths.iter().all(|path| !path.contains("tmux")),
        "default config search path must not include tmux locations: {paths:?}"
    );
}

#[cfg(unix)]
#[test]
fn tmux_fallback_paths_use_tmux_locations() {
    let _lock = crate::test_env::lock_blocking();
    let _disable = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", None);
    let _home = EnvVarGuard::set("HOME", Some("/tmp/rmux-home"));
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some("/tmp/rmux-xdg"));

    let paths = default_tmux_fallback_paths();

    assert_eq!(
        paths,
        vec![
            "/etc/tmux.conf".to_owned(),
            "/tmp/rmux-home/.tmux.conf".to_owned(),
            "/tmp/rmux-xdg/tmux/tmux.conf".to_owned(),
            "/tmp/rmux-home/.config/tmux/tmux.conf".to_owned(),
        ]
    );
    assert!(
        paths.iter().all(|path| !path.ends_with("rmux.conf")),
        "tmux fallback paths must not include rmux config files: {paths:?}"
    );
}

#[test]
fn tmux_fallback_paths_can_be_disabled_by_env() {
    let _lock = crate::test_env::lock_blocking();
    let _disable = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", Some("1"));

    assert!(default_tmux_fallback_paths().is_empty());
}

#[cfg(windows)]
#[test]
fn default_config_paths_use_documented_windows_locations() {
    let _lock = crate::test_env::lock_blocking();
    let _rmux_config = EnvVarGuard::set("RMUX_CONFIG_FILE", Some(r"C:\rmux\custom.conf"));
    let _appdata = EnvVarGuard::set("APPDATA", Some(r"C:\Users\tester\AppData\Roaming"));
    let _userprofile = EnvVarGuard::set("USERPROFILE", Some(r"C:\Users\tester"));
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some(r"C:\Users\tester\.config"));

    let paths = default_config_paths();

    assert_eq!(
        paths,
        vec![
            path_string(r"C:\Users\tester\.config\rmux\rmux.conf"),
            path_string(r"C:\Users\tester\.rmux.conf"),
            path_string(r"C:\Users\tester\AppData\Roaming\rmux\rmux.conf"),
            path_string(r"C:\rmux\custom.conf"),
        ]
    );
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.contains("rmux.conf"))
            .count(),
        3,
        "Windows search path must not add undocumented rmux.conf locations: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("tmux")),
        "Windows default config search path must not include tmux locations: {paths:?}"
    );
}

#[cfg(windows)]
#[test]
fn tmux_fallback_paths_use_documented_windows_tmux_locations() {
    let _lock = crate::test_env::lock_blocking();
    let _disable = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", None);
    let _appdata = EnvVarGuard::set("APPDATA", Some(r"C:\Users\tester\AppData\Roaming"));
    let _userprofile = EnvVarGuard::set("USERPROFILE", Some(r"C:\Users\tester"));
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some(r"C:\Users\tester\.config"));

    let paths = default_tmux_fallback_paths();

    assert_eq!(
        paths,
        vec![
            path_string(r"C:\Users\tester\.config\tmux\tmux.conf"),
            path_string(r"C:\Users\tester\.tmux.conf"),
            path_string(r"C:\Users\tester\AppData\Roaming\tmux\tmux.conf"),
        ]
    );
    assert!(
        paths.iter().all(|path| !path.ends_with("rmux.conf")),
        "tmux fallback paths must not include rmux config files: {paths:?}"
    );
}

#[cfg(windows)]
#[test]
fn windows_nul_config_path_is_empty() {
    let inputs = source_inputs_for_path("NUL", None, false, None, SourceReadPolicy::Strict)
        .expect("NUL should behave like an empty config file");
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].contents.is_empty());

    let inputs = source_inputs_for_path("nul", None, false, None, SourceReadPolicy::Strict)
        .expect("nul should be case-insensitive");
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].contents.is_empty());
}

#[cfg(windows)]
fn path_string(path: &str) -> String {
    std::path::PathBuf::from(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(unix)]
mod unix_identity {
    use super::{default_config_paths, EnvVarGuard};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn default_config_paths_keep_first_symlink_identity() {
        check_paths(Some(false));
    }

    #[test]
    fn default_config_paths_keep_first_hardlink_identity() {
        check_paths(Some(true));
    }

    #[test]
    fn default_config_paths_keep_distinct_equal_files_and_missing_paths() {
        check_paths(None);
    }

    fn check_paths(hardlink: Option<bool>) {
        let _lock = crate::test_env::lock_blocking();
        let fixture = Fixture::new();
        let first = fixture.0.join("home/.rmux.conf");
        let second = fixture.0.join("xdg/rmux/rmux.conf");
        fs::write(&first, "set -ga @identity X\n").expect("write first config");
        match hardlink {
            Some(true) => fs::hard_link(&first, &second).expect("hardlink config"),
            Some(false) => std::os::unix::fs::symlink(&first, &second).expect("symlink config"),
            None => {
                fs::copy(&first, &second).expect("copy distinct config");
            }
        }
        let home = fixture.0.join("home");
        let xdg = fixture.0.join("xdg");
        let _home = EnvVarGuard::set("HOME", home.to_str());
        let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", xdg.to_str());
        let missing = home.join(".config/rmux/rmux.conf");
        assert!(!missing.exists());

        let mut expected = vec![
            "/etc/rmux.conf".to_owned(),
            first.to_string_lossy().into_owned(),
        ];
        if hardlink.is_none() {
            expected.push(second.to_string_lossy().into_owned());
        }
        expected.push(missing.to_string_lossy().into_owned());
        assert_eq!(default_config_paths(), expected);
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "rmux-default-identity-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("home")).expect("create private config home");
            fs::create_dir_all(root.join("xdg/rmux")).expect("create private XDG config dir");
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(unix)]
#[test]
fn default_config_paths_characterizes_non_utf8_home() {
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn value_json(value: &Option<OsString>) -> serde_json::Value {
        serde_json::json!({
            "present": value.is_some(),
            "hex": value.as_ref().map(|value| hex(value.as_bytes())),
        })
    }

    // Keep the real environment lock through guard Drop and restoration checks.
    let _lock = crate::test_env::lock_blocking();
    if run_non_utf8_home_in_clean_child(
        "handler::scripting_support::config_path_tests::default_config_paths_characterizes_non_utf8_home",
    ) {
        return;
    }
    assert!(
        std::env::var_os("HOME").is_none(),
        "requires clean child HOME"
    );
    assert!(
        std::env::var_os("XDG_CONFIG_HOME").is_none(),
        "requires clean child XDG_CONFIG_HOME"
    );
    let fixture_root = PathBuf::from(
        std::env::var_os("RMUX_L073_FIXTURE_ROOT").expect("owned characterization fixture root"),
    );
    assert!(fixture_root.is_absolute());
    assert!(fixture_root.as_os_str().as_bytes().is_ascii());

    struct Case {
        id: &'static str,
        home: Option<&'static [u8]>,
        xdg: bool,
        expected: &'static [&'static [u8]],
    }
    // Frozen R2 cases: ordered raw paths, including the existing non-UTF8 split.
    // Relative byte paths are rooted in this case's owned ASCII directory.
    let cases: [Case; 8] = [
        Case {
            id: "HOME-absent-XDG-absent",
            home: None,
            xdg: false,
            expected: &[b"/etc/rmux.conf"],
        },
        Case {
            id: "HOME-absent-XDG-utf8",
            home: None,
            xdg: true,
            expected: &[b"/etc/rmux.conf", b"xdg/rmux/rmux.conf"],
        },
        Case {
            id: "HOME-empty-XDG-absent",
            home: Some(b""),
            xdg: false,
            expected: &[b"/etc/rmux.conf"],
        },
        Case {
            id: "HOME-empty-XDG-utf8",
            home: Some(b""),
            xdg: true,
            expected: &[b"/etc/rmux.conf", b"xdg/rmux/rmux.conf"],
        },
        Case {
            id: "HOME-utf8-XDG-absent",
            home: Some(b"home"),
            xdg: false,
            expected: &[
                b"/etc/rmux.conf",
                b"home/.rmux.conf",
                b"home/.config/rmux/rmux.conf",
            ],
        },
        Case {
            id: "HOME-utf8-XDG-utf8",
            home: Some(b"home"),
            xdg: true,
            expected: &[
                b"/etc/rmux.conf",
                b"home/.rmux.conf",
                b"xdg/rmux/rmux.conf",
                b"home/.config/rmux/rmux.conf",
            ],
        },
        Case {
            id: "HOME-non_utf8-XDG-absent",
            home: Some(b"home-\xff"),
            xdg: false,
            expected: &[b"/etc/rmux.conf"],
        },
        Case {
            id: "HOME-non_utf8-XDG-utf8",
            home: Some(b"home-\xff"),
            xdg: true,
            expected: &[b"/etc/rmux.conf", b"xdg/rmux/rmux.conf"],
        },
    ];
    let mut observed = BTreeSet::new();
    for case in cases {
        assert!(observed.insert(case.id), "duplicate characterization case");
        assert!(std::env::var_os("HOME").is_none());
        assert!(std::env::var_os("XDG_CONFIG_HOME").is_none());
        // Capture absence before installing any non-UTF8 bytes. Both existing
        // guards stay live across every fallible operation and resolver call.
        let home_guard = EnvVarGuard::set("HOME", None);
        let xdg_guard = EnvVarGuard::set("XDG_CONFIG_HOME", None);
        let case_root = fixture_root.join(case.id);
        std::fs::create_dir_all(&case_root).expect("create ASCII fixture root");
        let rooted = |relative: &[u8]| {
            let mut bytes = case_root.as_os_str().as_bytes().to_vec();
            bytes.push(b'/');
            bytes.extend_from_slice(relative);
            bytes
        };
        let home = case.home.map(|relative| {
            OsString::from_vec(if relative.is_empty() {
                Vec::new()
            } else {
                rooted(relative)
            })
        });
        let xdg = case.xdg.then(|| OsString::from_vec(rooted(b"xdg")));
        for (name, value) in [("HOME", &home), ("XDG_CONFIG_HOME", &xdg)] {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
            assert_eq!(std::env::var_os(name), *value);
        }
        let home_observed = std::env::var_os("HOME");
        let xdg_observed = std::env::var_os("XDG_CONFIG_HOME");
        let expected: Vec<Vec<u8>> = case
            .expected
            .iter()
            .map(|path| {
                if path.starts_with(b"/") {
                    path.to_vec()
                } else {
                    rooted(path)
                }
            })
            .collect();
        let mut owned_candidates = Vec::new();
        if let Some(value) = home.as_ref().filter(|value| !value.is_empty()) {
            let path = PathBuf::from(value);
            owned_candidates.push(path.join(".rmux.conf"));
            owned_candidates.push(path.join(".config/rmux/rmux.conf"));
        }
        if let Some(value) = &xdg {
            owned_candidates.push(PathBuf::from(value).join("rmux/rmux.conf"));
        }
        let assert_no_config_files = || {
            assert!(
                std::fs::read_dir(&case_root)
                    .expect("read owned fixture root")
                    .next()
                    .is_none(),
                "fixture root must stay empty; never create the non-UTF8 HOME"
            );
            for candidate in &owned_candidates {
                let error = std::fs::symlink_metadata(candidate)
                    .expect_err("owned config candidate must not exist");
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            }
        };
        assert_no_config_files();
        // Exactly one call to the real resolver for this frozen case.
        let actual: Vec<Vec<u8>> = default_config_paths()
            .iter()
            .map(|path| path.as_bytes().to_vec())
            .collect();
        assert_eq!(actual, expected, "case {}", case.id);
        assert_no_config_files();
        drop(xdg_guard);
        drop(home_guard);
        assert!(std::env::var_os("HOME").is_none());
        assert!(std::env::var_os("XDG_CONFIG_HOME").is_none());
        println!(
            "RMUX_L073_OBSERVATION {}",
            serde_json::json!({
                "case_id": case.id,
                "resolver": "startup",
                "pid": std::process::id(),
                "HOME": value_json(&home_observed),
                "XDG_CONFIG_HOME": value_json(&xdg_observed),
                "actual_paths_hex": actual.iter().map(|path| hex(path)).collect::<Vec<_>>(),
                "expected_paths_hex": expected.iter().map(|path| hex(path)).collect::<Vec<_>>(),
                "restored_absent": true,
            })
        );
    }
    assert_eq!(observed.len(), 8);
}

#[cfg(unix)]
#[test]
fn non_utf8_home_characterization_isolates_parent_environment() {
    use std::os::unix::ffi::OsStringExt;

    let parent_home = std::ffi::OsString::from_vec(b"/parent-home-\xff".to_vec());
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["handler::scripting_support::config_path_tests::default_config_paths_characterizes_non_utf8_home", "--exact", "--test-threads=1", "--nocapture"])
        .env("HOME", &parent_home)
        .env("XDG_CONFIG_HOME", "/parent-xdg")
        .env_remove("RMUX_L073_FIXTURE_ROOT")
        .output()
        .expect("run characterization with inherited parent environment");
    assert!(
        output.status.success(),
        "characterization must own its clean child: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.matches("RMUX_L073_OBSERVATION ").count(), 8);
}

#[cfg(unix)]
fn run_non_utf8_home_in_clean_child(test_name: &str) -> bool {
    use std::os::unix::{ffi::OsStrExt, fs::DirBuilderExt};
    use std::path::PathBuf;

    let names = ["HOME", "XDG_CONFIG_HOME", "RMUX_L073_FIXTURE_ROOT"];
    let parent = names.map(std::env::var_os);
    // The existing externally isolated recipe already owns a fixture.
    if parent[0].is_none() && parent[1].is_none() && parent[2].is_some() {
        return false;
    }
    let temporary = std::env::temp_dir();
    let base = if temporary.is_absolute() && temporary.as_os_str().as_bytes().is_ascii() {
        temporary
    } else {
        PathBuf::from("/tmp")
    };
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("fixture clock")
        .as_nanos();
    let root = base.join(format!("rmux-non-utf8-home-{}-{nonce}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("create private ASCII characterization fixture");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([test_name, "--exact", "--test-threads=1", "--nocapture"])
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("RMUX_L073_FIXTURE_ROOT", &root)
        .output()
        .expect("run clean characterization child");
    std::fs::remove_dir_all(&root).expect("remove owned characterization fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        names.map(std::env::var_os),
        parent,
        "parent environment changed"
    );
    assert!(
        output.status.success(),
        "clean characterization child failed"
    );
    assert_eq!(stdout.matches("RMUX_L073_OBSERVATION ").count(), 8);
    true
}
