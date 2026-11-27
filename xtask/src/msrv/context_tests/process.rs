use super::*;

const OVERRIDES: [&str; 3] = [
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTC_BOOTSTRAP",
];
const PARENT_PROBE: &str = "RMUX_MSRV_PARENT_OVERRIDE_PROBE";
const FIXTURE_CHILD: &str = "RMUX_MSRV_CONTEXT_FIXTURE_CHILD";

fn child(test: &str) -> Command {
    let executable = env::current_exe().unwrap();
    let mut command = match (
        env::var_os("RMUX_IDENTITY_PYTHON"),
        env::var_os("RMUX_IDENTITY_SCRIPT"),
    ) {
        (Some(python), Some(script)) => {
            let mut command = Command::new(python);
            command.arg(script).arg("run").arg(executable);
            command
        }
        _ => Command::new(executable),
    };
    command.args(["--exact", test, "--nocapture"]);
    command
}

pub(super) fn isolated() -> bool {
    let thread = std::thread::current();
    let test = thread.name().expect("named context test");
    if env::var_os(FIXTURE_CHILD).as_deref() == Some(std::ffi::OsStr::new(test)) {
        return false;
    }
    // These miniature workspaces exercise raw Cargo and exact-compiler rejection.
    // Isolate their parent process; changing a Cargo command alone cannot hide
    // an inherited override from context::check. Never mutate the test runner.
    let mut command = child(test);
    command.env(FIXTURE_CHILD, test);
    for name in OVERRIDES {
        command.env_remove(name);
    }
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("CARGO_TARGET_") && name.ends_with("_RUNNER") {
            command.env_remove(key);
        }
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{test}: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    true
}

#[test]
fn inherited_overrides_are_rejected_but_do_not_pollute_fixture_children() {
    if let Ok(key) = env::var(PARENT_PROBE) {
        assert!(OVERRIDES.contains(&key.as_str()));
        let mut fixture = Fixture::new("parent-override");
        for value in [None, Some("")] {
            let mut metadata = fixture.metadata();
            if let Some(value) = value {
                metadata.env(&key, value);
            } else {
                metadata.env_remove(&key);
            }
            let error = cargo_output(&mut metadata).unwrap_err().to_string();
            assert!(error.contains(&key), "{error}");
            for args in [vec!["check"], vec!["test", "--lib"], vec!["test", "--doc"]] {
                let mut command = fixture.command(&args);
                command.env_remove(&key);
                let error = fixture
                    .runner
                    .cell("parent-override", &mut command, None)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(&key), "{error}");
                assert_eq!(fixture.runner.count, 0);
            }
        }
        return;
    }
    let test = "msrv::context_tests::process::inherited_overrides_are_rejected_but_do_not_pollute_fixture_children";
    for key in OVERRIDES {
        for (selected, probe) in [
            (test, true),
            (
                "msrv::context_tests::unconfigured_metadata_check_test_and_doc_use_real_tools",
                false,
            ),
            (
                "msrv::context_tests::only_the_repository_task_alias_is_inert_configuration",
                false,
            ),
        ] {
            let mut command = child(selected);
            for name in OVERRIDES {
                command.env_remove(name);
            }
            command.env(key, "arbitrary-parent-override");
            if probe {
                command.env(PARENT_PROBE, key);
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "{key}: {selected}\n{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}
