use super::*;

fn metadata() -> Value {
    json!({"workspace_members": ["sdk", "ratatui"], "packages": [
        {"id": "sdk", "name": "rmux-sdk", "rust_version": FLOOR,
         "features": {"default": ["web"], "crossterm": [], "regex": [], "web": []}},
        {"id": "ratatui", "name": "ratatui-rmux", "rust_version": FLOOR, "features": {}}
    ]})
}

#[test]
fn absent_drifted_and_omitted_declarations_are_rejected() {
    for value in [Value::Null, json!("1.99.0"), json!("1.75.0")] {
        let mut data = metadata();
        data["packages"][0]["rust_version"] = value;
        assert!(cells::plan(&data, "native-host").is_err());
    }
    let mut data = metadata();
    data["workspace_members"]
        .as_array_mut()
        .unwrap()
        .push(json!("omitted"));
    assert!(cells::plan(&data, "native-host").is_err());
    let mut data = metadata();
    data["workspace_members"]
        .as_array_mut()
        .unwrap()
        .push(json!("sdk"));
    assert!(cells::plan(&data, "native-host").is_err());
}

#[test]
fn sdk_powerset_and_ratatui_selections_are_distinct_and_complete() {
    let plan = cells::plan(&metadata(), "native-host").unwrap();
    let sdk: Vec<_> = plan
        .iter()
        .filter(|c| c.id.starts_with("rmux-sdk-") && !c.args.contains(&"--all-targets".into()))
        .collect();
    assert_eq!(sdk.len(), 10);
    assert_eq!(
        plan.iter()
            .filter(|c| c.id.starts_with("ratatui-rmux-"))
            .count(),
        3
    );
    for selected in [
        "none",
        "web",
        "regex",
        "crossterm",
        "crossterm+regex",
        "crossterm+web",
        "regex+web",
        "crossterm+regex+web",
    ] {
        let cell = sdk
            .iter()
            .find(|c| c.id == format!("rmux-sdk-{selected}"))
            .unwrap();
        assert!(cell.args.contains(&"--no-default-features".into()));
        assert_eq!(cell.target, "native-host");
        assert!(!cell.args.contains(&"--all-targets".into()));
    }
    assert_eq!(
        plan.iter()
            .filter(|c| c.id.starts_with("workspace-targets-"))
            .count(),
        3
    );
    assert!(plan
        .iter()
        .any(|c| c.id == "crypto-wasm" && c.target == "wasm32-unknown-unknown"));
}

#[test]
fn no_default_sdk_targets_are_checked_without_workspace_feature_unification() {
    let plan = cells::plan(&metadata(), "native-host").unwrap();
    for (id, args) in [
        (
            "rmux-sdk-targets-none",
            cells::words(&[
                "check",
                "-p",
                "rmux-sdk",
                "--all-targets",
                "--no-default-features",
            ]),
        ),
        (
            "workspace-targets-none",
            cells::words(&[
                "check",
                "--workspace",
                "--all-targets",
                "--no-default-features",
            ]),
        ),
    ] {
        let matching: Vec<_> = plan.iter().filter(|cell| cell.id == id).collect();
        assert_eq!(
            matching.len(),
            1,
            "missing or duplicate no-default boundary: {id}"
        );
        assert_eq!(
            matching[0].args, args,
            "feature-unified or partial boundary: {id}"
        );
        assert_eq!(matching[0].target, "native-host");
    }
}

#[test]
fn newer_compiler_and_wrong_tool_are_not_exact_floor_proof() {
    exact_version("rustc", &format!("rustc {FLOOR} (hash date)")).unwrap();
    for version in [
        "rustc 1.96.1 (hash date)",
        "rustc 1.88.0-nightly (hash date)",
        "cargo 1.88.0",
    ] {
        assert!(exact_version("rustc", version).is_err());
    }
}

#[test]
fn server_powerset_checks_test_targets_without_workspace_feature_unification() {
    let mut data = metadata();
    data["workspace_members"]
        .as_array_mut()
        .unwrap()
        .push(json!("server"));
    data["packages"].as_array_mut().unwrap().push(json!({
        "id": "server", "name": "rmux-server", "rust_version": FLOOR,
        "features": {"default": ["web"], "fuzzing": [], "web": []}
    }));
    let plan = cells::plan(&data, "native-host").unwrap();
    let server: Vec<_> = plan
        .iter()
        .filter(|c| c.id.starts_with("rmux-server-"))
        .collect();
    assert_eq!(server.len(), 6);
    for cell in server {
        assert!(cell.args.contains(&"--all-targets".into()), "{}", cell.id);
        assert!(!cell.args.contains(&"--workspace".into()), "{}", cell.id);
    }
    let none = plan.iter().find(|c| c.id == "rmux-server-none").unwrap();
    assert!(none.args.contains(&"--no-default-features".into()));
    assert!(!none.args.contains(&"--features".into()));
}

#[test]
fn options_require_explicit_absolute_owned_paths() {
    assert!(Options::parse(cells::words(&["--toolchain-dir", "relative"]).into_iter()).is_err());
    assert!(Options::parse(cells::words(&["--ignore-rust-version"]).into_iter()).is_err());
    assert!(Options::parse(std::iter::empty()).is_err());
}
