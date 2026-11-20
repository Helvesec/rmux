//! Enumerate feature selections independently of Cargo's workspace unification.

use super::{invalid, Error, FLOOR};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Serialize)]
pub(super) struct Cell {
    pub(super) id: String,
    pub(super) target: String,
    pub(super) args: Vec<String>,
}

pub(super) fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

fn selections(features: &[&str]) -> Result<Vec<(String, Vec<String>)>, Error> {
    if features.len() > 8 {
        return Err(invalid(
            "more than eight combinable features: review MSRV matrix scope",
        ));
    }
    let mut selections = vec![
        ("default".into(), vec![]),
        ("all".into(), words(&["--all-features"])),
    ];
    for mask in 0..(1 << features.len()) {
        let selected: Vec<_> = features
            .iter()
            .enumerate()
            .filter_map(|(bit, name)| (mask & (1 << bit) != 0).then_some(*name))
            .collect();
        let label = if selected.is_empty() {
            "none".into()
        } else {
            selected.join("+")
        };
        let mut args = words(&["--no-default-features"]);
        if !selected.is_empty() {
            args.extend(words(&["--features", &selected.join(",")]));
        }
        selections.push((label, args));
    }
    Ok(selections)
}

pub(super) fn plan(metadata: &Value, host: &str) -> Result<Vec<Cell>, Error> {
    let members = metadata["workspace_members"]
        .as_array()
        .ok_or_else(|| invalid("missing workspace members"))?;
    let mut missing: BTreeSet<_> = members
        .iter()
        .map(|m| m.as_str().ok_or_else(|| invalid("invalid member ID")))
        .collect::<Result<_, _>>()?;
    if missing.len() != members.len() || missing.is_empty() {
        return Err(invalid("duplicate or empty workspace members"));
    }
    let mut packages = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or_else(|| invalid("missing packages"))?
    {
        let id = package["id"]
            .as_str()
            .ok_or_else(|| invalid("missing package ID"))?;
        if missing.remove(id) {
            if package["rust_version"].as_str() != Some(FLOOR) {
                return Err(invalid(format!(
                    "{} must declare rust-version {FLOOR}",
                    package["name"]
                )));
            }
            packages.push(package);
        }
    }
    if !missing.is_empty() {
        return Err(invalid("metadata omitted a workspace member"));
    }
    packages.sort_by_key(|p| p["name"].as_str());
    for facade in ["rmux-sdk", "ratatui-rmux"] {
        if !packages.iter().any(|p| p["name"] == facade) {
            return Err(invalid(format!("missing stable facade {facade}")));
        }
    }
    let mut plan = Vec::new();
    for package in packages {
        let name = package["name"]
            .as_str()
            .ok_or_else(|| invalid("missing package name"))?;
        let features: Vec<_> = package["features"]
            .as_object()
            .ok_or_else(|| invalid("missing features"))?
            .keys()
            .filter(|f| *f != "default")
            .map(String::as_str)
            .collect();
        for (label, flags) in selections(&features)? {
            let mut args = words(&["check", "-p", name]);
            if name == "rmux-server" {
                args.push("--all-targets".into());
            }
            args.extend(flags);
            plan.push(Cell {
                id: format!("{name}-{label}"),
                target: host.into(),
                args,
            });
        }
        if name == "rmux-sdk" {
            // Other members can enable SDK defaults even in a no-default workspace check.
            plan.push(Cell {
                id: "rmux-sdk-targets-none".into(),
                target: host.into(),
                args: words(&[
                    "check",
                    "-p",
                    name,
                    "--all-targets",
                    "--no-default-features",
                ]),
            });
        }
    }
    // Dev/build dependencies, tests, examples, benches and required-feature targets.
    for (label, flags) in selections(&[])? {
        let mut args = words(&["check", "--workspace", "--all-targets"]);
        args.extend(flags);
        plan.push(Cell {
            id: format!("workspace-targets-{label}"),
            target: host.into(),
            args,
        });
    }
    let tests = [
        ("xtask-tests", words(&["test", "-p", "xtask"])),
        (
            "sdk-values",
            words(&[
                "test",
                "-p",
                "rmux-sdk",
                "--all-features",
                "--test",
                "value_objects",
                "--test",
                "pane_state_types",
                "--test",
                "error_display",
            ]),
        ),
        (
            "ratatui-tests",
            words(&[
                "test",
                "-p",
                "ratatui-rmux",
                "--lib",
                "--test",
                "render",
                "--test",
                "state",
                "--test",
                "driver_revision_progression",
            ]),
        ),
        (
            "sdk-doc",
            words(&["test", "-p", "rmux-sdk", "--all-features", "--doc"]),
        ),
        (
            "ratatui-doc",
            words(&["test", "-p", "ratatui-rmux", "--doc"]),
        ),
    ];
    for (id, args) in tests {
        plan.push(Cell {
            id: id.into(),
            target: host.into(),
            args,
        });
    }
    for (label, flags) in selections(&[])? {
        let mut args = words(&["test", "-p", "rmux-sdk", "--lib"]);
        args.extend(flags);
        plan.push(Cell {
            id: format!("sdk-unit-{label}"),
            target: host.into(),
            args,
        });
    }
    plan.push(Cell {
        id: "crypto-wasm".into(),
        target: "wasm32-unknown-unknown".into(),
        args: words(&[
            "check",
            "-p",
            "rmux-web-crypto",
            "--lib",
            "--no-default-features",
            "--features",
            "wasm",
        ]),
    });
    Ok(plan)
}
