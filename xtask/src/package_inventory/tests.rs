use super::*;
use serde_json::{json, Value};

mod fixture;
mod provenance;
use fixture::Fixture;

const EXPECTED: &str = include_str!("../../spec/package-inventory-fixture-v1.json");

fn rejects_fixture(fixture: &Fixture, affected: &str) {
    let error = check(EXPECTED, fixture.packages()).expect_err("changed declarations must fail");
    assert!(error.to_string().contains(affected), "{error}");
}

fn cargo_json(fixture: &Fixture) -> Value {
    let output = execute(
        Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .current_dir(&fixture.root)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--locked",
                "--offline",
            ]),
    )
    .unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cargo_fixture_matches_frozen_baseline_and_declared_semantics() {
    let fixture = Fixture::new();
    let packages = fixture.packages();
    check(EXPECTED, fixture.packages()).unwrap();
    assert_eq!(
        packages.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["helper-pkg", "internal-pkg", "registry-pkg", "surface-a"]
    );
    assert_eq!(packages[0].publish, None); // Omission does not inherit workspace false.
    assert_eq!(packages[1].publish, Some(vec![])); // Explicit inheritance does.
    assert_eq!(
        packages[2].publish,
        Some(vec!["private-a".into(), "private-b".into()])
    );
    assert_eq!(packages[3].publish, None); // Explicit true overrides workspace false.
    assert_eq!(packages[1].rust_version, None);
    assert_eq!(packages[3].rust_version.as_deref(), Some("1.70"));
    assert!(!packages[1].features.contains_key("default"));
    assert_eq!(packages[2].features["default"], Vec::<String>::new());
    assert_eq!(packages[3].features["default"], ["alpha", "zeta"]);
    assert_eq!(packages[3].features["implicit"], ["dep:implicit"]);
    assert_eq!(packages[3].features["alpha"], ["alias?/std", "dep:alias"]);
    assert!(!packages[3].features.contains_key("alias"));
    assert_eq!(packages[3].targets.len(), 2); // Test/example targets are outside this slice.
    assert_eq!(packages[3].targets[1].name, "renamed_core");
    assert_eq!(packages[3].targets[1].crate_types, ["cdylib", "rlib"]);
    assert_eq!(packages[1].targets[0].kind, ["proc-macro"]);
}

#[test]
fn export_is_identical_across_roots_and_cargo_ordering() {
    let first = Fixture::new();
    let second = Fixture::new();
    let a = run(Mode::Export, &first.root).unwrap().unwrap();
    let b = run(Mode::Export, &second.root.join("alpha"))
        .unwrap()
        .unwrap();
    assert_eq!(a, b);
    assert!(!a.contains(&first.root.to_string_lossy().to_string()));
    assert!(!a.contains("path+file:"));
    let mut raw = cargo_json(&first);
    raw["workspace_members"].as_array_mut().unwrap().reverse();
    raw["packages"].as_array_mut().unwrap().reverse();
    for package in raw["packages"].as_array_mut().unwrap() {
        package["manifest_path"] = json!("Z:\\different-machine\\Cargo.toml");
        package["targets"].as_array_mut().unwrap().reverse();
        for target in package["targets"].as_array_mut().unwrap() {
            target["src_path"] = json!("Z:\\different-machine\\source.rs");
            for field in ["kind", "crate_types", "required-features"] {
                if let Some(items) = target.get_mut(field).and_then(Value::as_array_mut) {
                    items.reverse();
                }
            }
        }
        if let Some(registries) = package["publish"].as_array_mut() {
            registries.reverse();
        }
        for values in package["features"].as_object_mut().unwrap().values_mut() {
            values.as_array_mut().unwrap().reverse();
        }
    }
    check(
        EXPECTED,
        metadata::packages(serde_json::from_value(raw).unwrap())
            .unwrap()
            .packages,
    )
    .unwrap();
}

#[test]
fn incomplete_duplicate_and_unknown_cargo_metadata_fails_closed() {
    let fixture = Fixture::new();
    let raw = cargo_json(&fixture);
    for field in [
        "publish",
        "rust_version",
        "features",
        "targets",
        "manifest_path",
    ] {
        let mut broken = raw.clone();
        broken["packages"][0]
            .as_object_mut()
            .unwrap()
            .remove(field)
            .unwrap();
        assert!(
            serde_json::from_value::<metadata::Metadata>(broken).is_err(),
            "{field}"
        );
    }
    let mut missing_source = raw.clone();
    missing_source["packages"][0]["targets"][0]
        .as_object_mut()
        .unwrap()
        .remove("src_path");
    assert!(serde_json::from_value::<metadata::Metadata>(missing_source).is_err());
    let mut missing = raw.clone();
    missing["packages"].as_array_mut().unwrap().pop();
    assert!(metadata::packages(serde_json::from_value(missing).unwrap()).is_err());
    for field in ["packages", "workspace_members"] {
        let mut duplicate = raw.clone();
        let values = duplicate[field].as_array_mut().unwrap();
        values.push(values[0].clone());
        assert!(
            metadata::packages(serde_json::from_value(duplicate).unwrap()).is_err(),
            "{field}"
        );
    }
    let mut unknown = raw;
    unknown["packages"][0]["targets"][0]["kind"] = json!(["future-target-kind"]);
    assert!(metadata::packages(serde_json::from_value(unknown).unwrap()).is_err());
}

#[test]
fn noncanonical_feature_registry_and_target_ordering_fails() {
    let fixture = Fixture::new();
    for mutate in [
        |i: &mut Inventory| i.packages[3].features.get_mut("default").unwrap().reverse(),
        |i: &mut Inventory| i.packages[2].publish.as_mut().unwrap().reverse(),
        |i: &mut Inventory| i.packages[3].targets.reverse(),
        |i: &mut Inventory| i.packages[3].targets[1].crate_types.reverse(),
        |i: &mut Inventory| i.packages[3].targets[0].required_features.reverse(),
    ] {
        let mut baseline: Inventory = serde_json::from_str(EXPECTED).unwrap();
        mutate(&mut baseline);
        let broken = canonical(&baseline).unwrap();
        assert_ne!(broken, EXPECTED);
        assert!(check(&broken, fixture.packages()).is_err());
    }
}

#[test]
fn missing_workspace_member_fails() {
    let fixture = Fixture::new();
    fixture.replace("Cargo.toml", "\"registry\", ", "");
    rejects_fixture(&fixture, "removed package:registry-pkg");
}

#[test]
fn added_workspace_member_fails() {
    let fixture = Fixture::new();
    fixture.replace("Cargo.toml", "\"registry\",", "\"extra\", \"registry\",");
    fixture.write(
        "extra/Cargo.toml",
        "[package]\nname = \"extra-pkg\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    fixture.write("extra/src/lib.rs", "");
    rejects_fixture(&fixture, "added package:extra-pkg");
}

#[test]
fn explicit_inherited_and_registry_publication_changes_fail() {
    for (file, before, after, affected) in [
        (
            "alpha/Cargo.toml",
            "publish = true",
            "publish = false",
            "surface-a",
        ),
        (
            "Cargo.toml",
            "publish = false",
            "publish = true",
            "internal-pkg",
        ),
        (
            "Cargo.toml",
            "publish = false",
            "publish = [\"inherited-registry\"]",
            "internal-pkg",
        ),
        (
            "registry/Cargo.toml",
            "\"private-b\", \"private-a\"",
            "\"private-a\"",
            "registry-pkg",
        ),
        (
            "helper/Cargo.toml",
            "edition = \"2021\"",
            "edition = \"2021\"\npublish = false",
            "helper-pkg",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.replace(file, before, after);
        rejects_fixture(&fixture, affected);
    }
}

#[test]
fn equivalent_explicit_and_inherited_publication_is_normalized() {
    let fixture = Fixture::new();
    fixture.replace(
        "internal/Cargo.toml",
        "publish.workspace = true",
        "publish = false",
    );
    fixture.replace(
        "helper/Cargo.toml",
        "edition = \"2021\"",
        "edition = \"2021\"\npublish = true",
    );
    check(EXPECTED, fixture.packages()).unwrap();
}

#[test]
fn added_and_removed_features_fail() {
    for (before, after) in [
        ("spare = []", "spare = []\nnew-feature = []"),
        ("spare = []", ""),
    ] {
        let fixture = Fixture::new();
        fixture.replace("alpha/Cargo.toml", before, after);
        rejects_fixture(&fixture, "surface-a");
    }
}

#[test]
fn feature_expansion_and_default_changes_fail() {
    for (file, before, after) in [
        (
            "alpha/Cargo.toml",
            "default = [\"zeta\", \"alpha\"]",
            "default = [\"alpha\"]",
        ),
        (
            "alpha/Cargo.toml",
            "alpha = [\"dep:alias\", \"alias?/std\"]",
            "alpha = [\"dep:alias\"]",
        ),
        ("registry/Cargo.toml", "default = []", ""),
        (
            "internal/Cargo.toml",
            "proc-macro = true",
            "proc-macro = true\n[features]\ndefault = []",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.replace(file, before, after);
        rejects_fixture(&fixture, "changed package:");
    }
}

#[test]
fn library_binary_kind_name_and_required_feature_changes_fail() {
    for (before, after) in [
        ("name = \"renamed_core\"", "name = \"different_lib\""),
        ("[\"rlib\", \"cdylib\"]", "[\"rlib\"]"),
        ("name = \"alpha-tool\"", "name = \"other-tool\""),
        (
            "required-features = [\"zeta\", \"alpha\"]",
            "required-features = [\"alpha\"]",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.replace("alpha/Cargo.toml", before, after);
        rejects_fixture(&fixture, "surface-a");
    }
    let fixture = Fixture::new();
    fixture.replace(
        "internal/Cargo.toml",
        "proc-macro = true",
        "proc-macro = false",
    );
    rejects_fixture(&fixture, "internal-pkg");
}

#[test]
fn added_and_removed_library_binary_targets_fail() {
    for path in [
        "helper/src/main.rs",
        "alpha/src/bin/extra.rs",
        "registry/src/lib.rs",
    ] {
        let fixture = Fixture::new();
        fixture.write(path, "fn main() {}\n");
        rejects_fixture(&fixture, "changed package:");
    }
    let fixture = Fixture::new();
    fixture.replace(
        "alpha/Cargo.toml",
        "[lib]\nname = \"renamed_core\"\ncrate-type = [\"rlib\", \"cdylib\"]\n",
        "",
    );
    fs::remove_file(fixture.root.join("alpha/src/lib.rs")).unwrap();
    rejects_fixture(&fixture, "surface-a");
    let fixture = Fixture::new();
    fixture.replace("alpha/Cargo.toml", "[[bin]]\nname = \"alpha-tool\"\npath = \"src/main.rs\"\nrequired-features = [\"zeta\", \"alpha\"]\n", "");
    fs::remove_file(fixture.root.join("alpha/src/main.rs")).unwrap();
    rejects_fixture(&fixture, "surface-a");
}

#[test]
fn identity_edition_version_and_declared_msrv_changes_fail() {
    for (before, after) in [
        ("name = \"surface-a\"", "name = \"renamed-package\""),
        ("\nversion.workspace = true", "\nversion = \"2.0.0\""),
        (
            "edition.workspace = true",
            "edition = \"2024\"\nrust-version = \"1.85\"",
        ),
        ("rust-version.workspace = true", "rust-version = \"1.71\""),
        ("rust-version.workspace = true", ""),
    ] {
        let fixture = Fixture::new();
        if after.contains("2024") {
            fixture.replace("alpha/Cargo.toml", "rust-version.workspace = true", "");
        }
        fixture.replace("alpha/Cargo.toml", before, after);
        rejects_fixture(&fixture, "package:surface-a");
    }
}

#[test]
fn malformed_incomplete_and_noncanonical_baselines_fail() {
    let fixture = Fixture::new();
    for broken in ["", "{}", "{", "null"] {
        assert!(check(broken, fixture.packages()).is_err());
    }
    for pointer in [
        "/source_base",
        "/scope",
        "/packages/0/publish",
        "/packages/0/rust_version",
        "/packages/0/features",
        "/packages/0/targets",
        "/packages/0/targets/0/required_features",
    ] {
        let mut value: Value = serde_json::from_str(EXPECTED).unwrap();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        value
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key)
            .unwrap();
        assert!(
            check(
                &(serde_json::to_string_pretty(&value).unwrap() + "\n"),
                fixture.packages()
            )
            .is_err(),
            "{pointer}"
        );
    }
    for (pointer, replacement) in [
        ("/schema_version", json!(2)),
        ("/source_base/commit", json!("HEAD")),
        ("/packages", json!([])),
        ("/packages/0/id", json!("/absolute/root/helper-pkg")),
        ("/packages/0/publish", json!(false)),
    ] {
        let mut value: Value = serde_json::from_str(EXPECTED).unwrap();
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            check(
                &(serde_json::to_string_pretty(&value).unwrap() + "\n"),
                fixture.packages()
            )
            .is_err(),
            "{pointer}"
        );
    }
    let mut reversed: Inventory = serde_json::from_str(EXPECTED).unwrap();
    reversed.packages.reverse();
    assert!(check(&canonical(&reversed).unwrap(), fixture.packages()).is_err());
    for broken in [
        EXPECTED.replace("\"std\": []", "\"std\": [], \"std\": []"),
        EXPECTED.replace(
            "\"edition\":",
            "\"machine_path\": \"/tmp/root\", \"edition\":",
        ),
        EXPECTED.trim_end().to_owned(),
    ] {
        assert_ne!(broken, EXPECTED);
        assert!(check(&broken, fixture.packages()).is_err());
    }
}

#[test]
fn check_is_read_only_update_is_explicit_and_source_is_historical() {
    let fixture = Fixture::new();
    assert!(run(Mode::Check, &fixture.root).is_err()); // Missing baseline is never created.
    assert!(!fixture.root.join(BASELINE_PATH).exists());
    let exported = run(Mode::Export, &fixture.root).unwrap().unwrap();
    run(Mode::Update, &fixture.root).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join(BASELINE_PATH)).unwrap(),
        exported
    );
    run(Mode::Check, &fixture.root).unwrap();
    fixture.replace(
        "registry/Cargo.toml",
        "default = []",
        "default = []\nadded = []",
    );
    assert!(run(Mode::Check, &fixture.root).is_err());
    assert!(run(Mode::Update, &fixture.root).is_err());
    assert!(run(Mode::Export, &fixture.root).is_err());
    assert_eq!(
        fs::read_to_string(fixture.root.join(BASELINE_PATH)).unwrap(),
        exported
    );
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "--quiet", "-m", "Change declarations"]);
    assert!(run(Mode::Check, &fixture.root).is_err()); // New HEAD does not bless changes.
    run(Mode::Update, &fixture.root).unwrap();
    run(Mode::Check, &fixture.root).unwrap();
    let updated = fs::read_to_string(fixture.root.join(BASELINE_PATH)).unwrap();
    assert_ne!(updated, exported);
    let source: Inventory = serde_json::from_str(&updated).unwrap();
    assert_eq!(
        source.source_base.commit,
        fixture.git(&["rev-parse", "HEAD"])
    );
    assert_eq!(
        source.source_base.tree,
        fixture.git(&["rev-parse", "HEAD^{tree}"])
    );
    fixture.write("unrelated.txt", "unrelated to the package contract\n");
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "--quiet", "-m", "Unrelated later commit"]);
    run(Mode::Check, &fixture.root).unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join(BASELINE_PATH)).unwrap(),
        updated
    );
}
