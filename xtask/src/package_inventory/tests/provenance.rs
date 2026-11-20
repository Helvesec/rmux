use super::*;

fn commit(fixture: &Fixture) {
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "--quiet", "-m", "Inventory input fixture"]);
}

fn ignore(fixture: &Fixture, patterns: &str) {
    let existing = fs::read_to_string(fixture.root.join(".gitignore")).unwrap();
    fixture.write(".gitignore", &(existing + patterns));
}

fn rejects_writers_without_writing(fixture: &Fixture, expected_input: &str) {
    assert!(fixture.git(&["status", "--porcelain"]).is_empty());
    let path = fixture.root.join(BASELINE_PATH);
    let before = fs::read(&path).unwrap();
    // Exercise both operations even if one incorrectly succeeds, and retain
    // their write observations before asserting either result.
    let export = run(Mode::Export, &fixture.root);
    let after_export = fs::read(&path).unwrap();
    let update = run(Mode::Update, &fixture.root);
    let after_update = fs::read(&path).unwrap();
    eprintln!("input={expected_input}; export_rejected={}; update_rejected={}; export_preserved={}; update_preserved={}",
        export.is_err(), update.is_err(), after_export == before, after_update == before);
    assert!(export.is_err(), "export accepted {expected_input}");
    assert!(update.is_err(), "update accepted {expected_input}");
    for error in [export.unwrap_err(), update.unwrap_err()] {
        assert!(error.to_string().contains(expected_input), "{error}");
    }
    assert_eq!(after_export, before);
    assert_eq!(after_update, before);
    fs::remove_file(&path).unwrap();
    assert!(run(Mode::Update, &fixture.root).is_err());
    assert!(!path.exists(), "failed update created an expected file");
}

#[test]
fn ignored_member_from_tracked_glob_rejects_export_and_update() {
    let fixture = Fixture::new();
    fixture.replace("Cargo.toml", "\"alpha\"]", "\"alpha*\"]");
    ignore(&fixture, "/alpha-local/\n");
    commit(&fixture);
    run(Mode::Update, &fixture.root).unwrap();
    let before = fs::read(fixture.root.join(BASELINE_PATH)).unwrap();
    fixture.write(
        "alpha-local/Cargo.toml",
        "[package]\nname = \"local-uncommitted\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    fixture.write("alpha-local/src/lib.rs", "");
    assert!(fixture
        .packages()
        .iter()
        .any(|p| p.name == "local-uncommitted"));
    assert!(run(Mode::Check, &fixture.root).is_err());
    assert_eq!(fs::read(fixture.root.join(BASELINE_PATH)).unwrap(), before);
    rejects_writers_without_writing(&fixture, "Cargo.toml");
}

#[test]
fn ignored_automatic_binary_rejects_export_and_update() {
    let fixture = Fixture::new();
    ignore(&fixture, "/alpha/src/bin/\n");
    commit(&fixture);
    run(Mode::Update, &fixture.root).unwrap();
    let before = fs::read(fixture.root.join(BASELINE_PATH)).unwrap();
    fixture.write("alpha/src/bin/uncommitted.rs", "fn main() {}\n");
    assert!(fixture
        .packages()
        .iter()
        .any(|p| p.targets.iter().any(|t| t.name == "uncommitted")));
    assert!(run(Mode::Check, &fixture.root).is_err());
    assert_eq!(fs::read(fixture.root.join(BASELINE_PATH)).unwrap(), before);
    rejects_writers_without_writing(&fixture, "uncommitted.rs");
}

#[test]
fn tracked_inputs_allow_unrelated_ignored_audit_and_cache() {
    let fixture = Fixture::new();
    ignore(&fixture, "/.scratch/\n/target/\n/alpha/examples/local.rs\n");
    fixture.write("alpha/src/bin/tracked.rs", "fn main() {}\n");
    commit(&fixture);
    let before = run(Mode::Export, &fixture.root).unwrap().unwrap();
    fixture.write(".scratch/Cargo.toml", "unrelated audit data\n");
    fixture.write("target/debug/cache.rs", "unrelated cache data\n");
    fixture.write("alpha/examples/local.rs", "fn main() {}\n");
    assert_eq!(run(Mode::Export, &fixture.root).unwrap().unwrap(), before);
    run(Mode::Update, &fixture.root).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join(BASELINE_PATH)).unwrap(),
        before
    );
    run(Mode::Check, &fixture.root.join("alpha")).unwrap();
    let inventory: Inventory = serde_json::from_str(&before).unwrap();
    assert_eq!(
        inventory.source_base.commit,
        fixture.git(&["rev-parse", "HEAD"])
    );
    assert_eq!(
        inventory.source_base.tree,
        fixture.git(&["rev-parse", "HEAD^{tree}"])
    );
    assert!(inventory
        .packages
        .iter()
        .any(|p| p.targets.iter().any(|t| t.name == "tracked")));
}

#[test]
fn explicit_target_inputs_require_committed_ownership() {
    let external = Fixture::new();
    for (input, name) in [
        (".scratch/local.rs".to_owned(), "local.rs"),
        (
            format!(
                "../{}/alpha/src/main.rs",
                external.root.file_name().unwrap().to_str().unwrap()
            ),
            "main.rs",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.replace(
            "alpha/Cargo.toml",
            "publish = true",
            "publish = true\nautobins = false",
        );
        commit(&fixture);
        run(Mode::Update, &fixture.root).unwrap();
        ignore(&fixture, "/.scratch/\n");
        fixture.write(".scratch/local.rs", "fn main() {}\n");
        fixture.replace(
            "alpha/Cargo.toml",
            "path = \"src/main.rs\"",
            &format!("path = \"../{input}\""),
        );
        commit(&fixture);
        // Paths do not enter the exported surface; check keeps its historical
        // provenance and still compares only the declarations.
        run(Mode::Check, &fixture.root).unwrap();
        rejects_writers_without_writing(&fixture, name);
    }
}

#[test]
fn tracked_target_path_with_parent_components_is_owned() {
    let fixture = Fixture::new();
    fixture.replace(
        "alpha/Cargo.toml",
        "path = \"src/main.rs\"",
        "path = \"../registry/src/main.rs\"",
    );
    commit(&fixture);
    run(Mode::Update, &fixture.root).unwrap();
    run(Mode::Check, &fixture.root).unwrap();
    run(Mode::Export, &fixture.root).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_inventory_inputs_require_committed_referents() {
    use std::os::unix::fs::symlink;
    let external = Fixture::new();
    for (input, target) in [
        ("alpha/src/main.rs", ".scratch/local.rs".to_owned()),
        ("alpha/Cargo.toml", ".scratch/local.toml".to_owned()),
        ("Cargo.toml", ".scratch/workspace.toml".to_owned()),
        (
            "alpha/src/main.rs",
            external
                .root
                .join("alpha/src/main.rs")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
    ] {
        let fixture = Fixture::new();
        run(Mode::Update, &fixture.root).unwrap();
        ignore(&fixture, "/.scratch/\n");
        let content = fs::read_to_string(fixture.root.join(input)).unwrap();
        if !Path::new(&target).is_absolute() {
            fixture.write(&target, &content);
        }
        fs::remove_file(fixture.root.join(input)).unwrap();
        let referent = if Path::new(&target).is_absolute() {
            PathBuf::from(&target)
        } else {
            let mut relative = PathBuf::new();
            for _ in fixture
                .root
                .join(input)
                .parent()
                .unwrap()
                .strip_prefix(&fixture.root)
                .unwrap()
                .components()
            {
                relative.push("..");
            }
            relative.join(&target)
        };
        symlink(referent, fixture.root.join(input)).unwrap();
        commit(&fixture);
        rejects_writers_without_writing(
            &fixture,
            Path::new(&target).file_name().unwrap().to_str().unwrap(),
        );
    }
}

#[cfg(unix)]
#[test]
fn committed_symlinks_and_workspace_root_alias_are_owned() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    ignore(&fixture, "/.scratch/\n");
    fs::remove_file(fixture.root.join("alpha/src/main.rs")).unwrap();
    symlink(
        "../../registry/src/main.rs",
        fixture.root.join("alpha/src/main.rs"),
    )
    .unwrap();
    symlink("registry", fixture.root.join("alias")).unwrap();
    fixture.replace(
        "alpha/Cargo.toml",
        "path = \"src/main.rs\"",
        "path = \"../alias/../alpha/src/main.rs\"",
    );
    commit(&fixture);
    let exported = run(Mode::Export, &fixture.root).unwrap().unwrap();
    fs::create_dir_all(fixture.root.join(".scratch")).unwrap();
    let alias = fixture.root.join(".scratch/root-link");
    symlink(fs::canonicalize(&fixture.root).unwrap(), &alias).unwrap();
    assert_eq!(run(Mode::Export, &alias).unwrap().unwrap(), exported);
    run(Mode::Update, &alias).unwrap();
    run(Mode::Check, &fixture.root).unwrap();
}
