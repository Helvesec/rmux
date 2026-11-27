//! Acceptance of the public registry documentation command.
use rmux_core::{command_inventory::RMUX_EXTENSION_COMMANDS, command_parser::COMMAND_TABLE};
use std::{fs, path::Path, process::Command};

fn invoke(output: &Path, check: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command.args(["registry-docs", "--output-dir"]).arg(output);
    if check {
        command.arg("--check");
    }
    command.output().expect("execute xtask")
}

#[test]
fn registry_docs_projects_implemented_commands_and_rejects_stale_output() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let output = root
        .join("target")
        .join("tmp")
        .join(format!("docs-acceptance-{}", std::process::id()));
    assert!(!output.exists(), "fixture must be private and new");
    let generated = invoke(&output, false);
    assert!(
        generated.status.success(),
        "missing registry documentation generator: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let manual = fs::read_to_string(output.join("rmux.1")).unwrap();
    let installed = include_str!("../../docs/man/rmux.1");
    let notes = installed.split_once(".SH NOTES\n").unwrap().1;
    assert!(notes.contains(".B display-popup\n"));
    assert!(notes.contains(".B link-window\n"));
    assert!(notes.contains(".B unlink-window\n"));
    assert_eq!(
        manual.split_once(".SH NOTES\n").unwrap().1,
        notes,
        "generator must retain the complete installed notes, including popup and links"
    );
    assert_eq!(
        manual, installed,
        "installed manual differs from generation"
    );
    let cli = fs::read_to_string(output.join("cli-reference.md")).unwrap();
    let expected: Vec<_> = COMMAND_TABLE
        .iter()
        .chain(RMUX_EXTENSION_COMMANDS.iter())
        .map(|entry| entry.name)
        .collect();
    assert_eq!(expected.len(), 111);
    let actual: Vec<_> = cli
        .lines()
        .filter_map(|line| line.strip_prefix("## `").and_then(|s| s.strip_suffix('`')))
        .collect();
    assert_eq!(actual, expected, "exact implemented registry order");
    let v2 = actual
        .iter()
        .position(|name| *name == "export-window-snapshot-v2")
        .unwrap();
    let v1 = actual
        .iter()
        .position(|name| *name == "export-window-snapshot")
        .unwrap();
    assert!(v2 < v1);
    let names = [
        "cli-reference.md",
        "configuration-reference.md",
        "rmux.1",
        "compliance.md",
        "compliance.json",
    ];
    let originals: Vec<_> = names
        .iter()
        .map(|name| fs::read(output.join(name)).unwrap())
        .collect();
    assert_eq!(
        fs::read(root.join("docs/man/rmux.1")).unwrap(),
        originals[2],
        "installed manual must match the current registry projection"
    );
    assert!(invoke(&output, true).status.success());
    assert!(invoke(&output, false).status.success());
    for (name, original) in names.iter().zip(&originals) {
        assert_eq!(
            fs::read(output.join(name)).unwrap(),
            *original,
            "deterministic {name}"
        );
    }
    for (name, original) in names.iter().zip(&originals) {
        fs::write(output.join(name), b"stale projection\n").unwrap();
        let stale = invoke(&output, true);
        assert!(
            !stale.status.success(),
            "checker must reject drift in {name}"
        );
        assert!(
            String::from_utf8_lossy(&stale.stderr).contains(name),
            "identify stale file"
        );
        assert_eq!(
            fs::read(output.join(name)).unwrap(),
            b"stale projection\n",
            "check is read only"
        );
        fs::write(output.join(name), original).unwrap();
    }
    assert!(invoke(&output, true).status.success());
    fs::remove_dir_all(output).unwrap();
}

fn stderr_identifies_manual(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr)
        .replace('\\', "/")
        .contains("docs/man/rmux.1")
}

#[test]
fn stale_manual_diagnostic_accepts_native_path_separators() {
    for stderr in [
        "stale or missing registry projection: /repo/docs/man/rmux.1",
        r"stale or missing registry projection: C:\repo\docs\man\rmux.1",
        r"stale or missing registry projection: C:\repo\docs/man\rmux.1",
    ] {
        assert!(
            stderr_identifies_manual(stderr.as_bytes()),
            "stderr: {stderr}"
        );
    }
    for stderr in [
        "stale or missing registry projection: /repo/docs/generated/rmux.1",
        r"stale or missing registry projection: C:\repo\docs\man\other.1",
        "registry-docs requires a repository source root",
    ] {
        assert!(
            !stderr_identifies_manual(stderr.as_bytes()),
            "stderr: {stderr}"
        );
    }
}

#[test]
fn checker_reads_current_authority_and_preserves_output_on_invalid_input() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let source = root
        .join("target")
        .join("tmp")
        .join(format!("docs-authority-{}", std::process::id()));
    let ledger_path = source.join("tests/reference/tmux_compat/divergences.toml");
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    fs::write(source.join("probe.rs"), "fn measured_probe() {}\n").unwrap();
    let text = "schema = 1\n[policy]\noracle = 'tmux 3.7b'\nunlisted_divergences_are_bugs = true\n[[entry]]\nid = 'C-D1'\nsummary = 'fixture authority'\ninventory = 'bounded'\ntests = ['probe.rs::measured_probe']\n";
    fs::write(&ledger_path, text).unwrap();
    let output = source.join("output");
    let invoke = |check: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .current_dir(&source)
            .args(["registry-docs", "--output-dir"])
            .arg(&output);
        if check {
            command.arg("--check");
        }
        command.output().unwrap()
    };
    let generated = invoke(false);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let original = fs::read(output.join("compliance.json")).unwrap();
    assert!(String::from_utf8_lossy(&original).contains("fixture authority"));
    assert!(invoke(true).status.success());
    for mutation in [
        text.replace("tmux 3.7b", "tmux 3.4"),
        text.replace("C-D1", "C-D01"),
        text.replace("probe.rs", "missing.rs"),
        text.replace("measured_probe", "missing_probe"),
        text.replace("fixture authority", "changed authority"),
    ] {
        fs::write(&ledger_path, mutation).unwrap();
        assert!(!invoke(true).status.success());
        assert_eq!(fs::read(output.join("compliance.json")).unwrap(), original);
    }
    fs::write(&ledger_path, text).unwrap();
    assert!(invoke(true).status.success());
    let installed = |check: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .current_dir(&source)
            .args(["registry-docs", "--output-dir", "docs/generated"]);
        if check {
            command.arg("--check");
        }
        command.output().unwrap()
    };
    assert!(installed(false).status.success());
    assert!(installed(true).status.success());
    let man = source.join("docs/man/rmux.1");
    assert_eq!(
        fs::read(&man).unwrap(),
        fs::read(source.join("docs/generated/rmux.1")).unwrap()
    );
    fs::write(&man, "unowned manual\n").unwrap();
    let stale = installed(true);
    assert!(!stale.status.success());
    assert!(
        stderr_identifies_manual(&stale.stderr),
        "identify stale manual; stderr:\n{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert!(!installed(false).status.success());
    assert_eq!(fs::read_to_string(&man).unwrap(), "unowned manual\n");
    fs::remove_dir_all(source).unwrap();
}
