//! SDK identity re-export contract.
//!
//! These tests prove the four authoritative identity newtypes
//! (`SessionName`, `SessionId`, `WindowId`, `PaneId`) reach SDK users
//! through `rmux_sdk` only, with no SDK-side redeclaration. They also
//! pin the `rmux-sdk` Cargo manifest so the SDK never gains a normal
//! dependency on `rmux-core`, `rmux-server`, `rmux-client`, or
//! `rmux-pty`.

#![allow(dead_code, clippy::extra_unused_type_parameters)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rmux_sdk::{PaneId, SessionId, SessionName, WindowId};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn assert_static<T: 'static>() {}
fn assert_clone<T: Clone>() {}
fn assert_copy<T: Copy>() {}
fn assert_eq_hash<T: Eq + Hash>() {}
fn assert_debug<T: std::fmt::Debug>() {}
fn assert_display<T: std::fmt::Display>() {}

fn assert_same_type<T, U>()
where
    T: 'static,
    U: 'static,
{
    assert_eq!(
        std::any::TypeId::of::<T>(),
        std::any::TypeId::of::<U>(),
        "the SDK re-export must resolve to the rmux-proto definition"
    );
}

fn _assert_bounds() {
    assert_send::<SessionName>();
    assert_sync::<SessionName>();
    assert_static::<SessionName>();
    assert_clone::<SessionName>();
    assert_eq_hash::<SessionName>();
    assert_debug::<SessionName>();
    assert_display::<SessionName>();

    for id_check in [
        || {
            assert_send::<SessionId>();
            assert_sync::<SessionId>();
            assert_static::<SessionId>();
            assert_copy::<SessionId>();
            assert_clone::<SessionId>();
            assert_eq_hash::<SessionId>();
            assert_debug::<SessionId>();
            assert_display::<SessionId>();
        },
        || {
            assert_send::<WindowId>();
            assert_sync::<WindowId>();
            assert_static::<WindowId>();
            assert_copy::<WindowId>();
            assert_clone::<WindowId>();
            assert_eq_hash::<WindowId>();
            assert_debug::<WindowId>();
            assert_display::<WindowId>();
        },
        || {
            assert_send::<PaneId>();
            assert_sync::<PaneId>();
            assert_static::<PaneId>();
            assert_copy::<PaneId>();
            assert_clone::<PaneId>();
            assert_eq_hash::<PaneId>();
            assert_debug::<PaneId>();
            assert_display::<PaneId>();
        },
    ] {
        id_check();
    }
}

#[test]
fn sdk_re_exports_resolve_to_rmux_proto_identity_types() {
    assert_same_type::<SessionName, rmux_proto::SessionName>();
    assert_same_type::<SessionId, rmux_proto::SessionId>();
    assert_same_type::<WindowId, rmux_proto::WindowId>();
    assert_same_type::<PaneId, rmux_proto::PaneId>();
}

#[test]
fn session_id_displays_with_dollar_prefix() {
    let id = SessionId::new(7);
    assert_eq!(id.to_string(), "$7");
    assert_eq!(id.as_u32(), 7);
    assert_eq!(SessionId::new(7), SessionId::from(7_u32));
}

#[test]
fn window_id_displays_with_at_prefix() {
    let id = WindowId::new(2);
    assert_eq!(id.to_string(), "@2");
    assert_eq!(id.as_u32(), 2);
}

#[test]
fn pane_id_displays_with_percent_prefix() {
    let id = PaneId::new(13);
    assert_eq!(id.to_string(), "%13");
    assert_eq!(id.as_u32(), 13);
}

#[test]
fn session_name_validation_rejects_empty_value_via_sdk_re_export() {
    assert!(SessionName::new("").is_err());
    let rewritten = SessionName::new("alpha:beta.gamma").expect("rewrites colons and dots");
    assert_eq!(rewritten.as_str(), "alpha_beta_gamma");
}

#[test]
fn identity_newtypes_hash_consistently_across_clones() {
    let pane = PaneId::new(5);
    let mut hasher_a = DefaultHasher::new();
    pane.hash(&mut hasher_a);
    let mut hasher_b = DefaultHasher::new();
    pane.hash(&mut hasher_b);
    assert_eq!(hasher_a.finish(), hasher_b.finish());
}

/// Splits a Cargo.toml string into `(header, body)` pairs, one per
/// `[section]` block, preserving the original section order so the caller
/// can assert truthful per-section invariants instead of scanning the
/// whole file.
fn parse_manifest_sections(manifest: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_body = String::new();

    for raw_line in manifest.lines() {
        let trimmed = raw_line.trim_start();
        let is_header =
            trimmed.starts_with('[') && !trimmed.starts_with("[[") && trimmed.contains(']');

        if is_header {
            if let Some(header) = current_header.take() {
                sections.push((header, std::mem::take(&mut current_body)));
            }
            let header_text = trimmed
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            current_header = Some(header_text);
        } else if current_header.is_some() {
            current_body.push_str(raw_line);
            current_body.push('\n');
        }
    }

    if let Some(header) = current_header {
        sections.push((header, current_body));
    }

    sections
}

fn header_is_normal_dependencies(header: &str) -> bool {
    if header == "dependencies" {
        return true;
    }
    if let Some(rest) = header.strip_prefix("target.") {
        return rest.ends_with(".dependencies");
    }
    false
}

fn header_is_any_dependency_section(header: &str) -> bool {
    if matches!(
        header,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    ) {
        return true;
    }
    if let Some(rest) = header.strip_prefix("target.") {
        return rest.ends_with(".dependencies")
            || rest.ends_with(".dev-dependencies")
            || rest.ends_with(".build-dependencies");
    }
    false
}

const FORBIDDEN_INTERNAL_CRATES: [&str; 4] =
    ["rmux-core", "rmux-server", "rmux-client", "rmux-pty"];

fn body_mentions_crate(body: &str, crate_name: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }
        let key = trimmed.split('=').next().unwrap_or_default().trim();
        let key = key.trim_matches('"');
        key == crate_name
    })
}

fn dependency_line<'a>(body: &'a str, crate_name: &str) -> Option<&'a str> {
    body.lines().find(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }
        let key = trimmed.split('=').next().unwrap_or_default().trim();
        key.trim_matches('"') == crate_name
    })
}

#[test]
fn rmux_sdk_cargo_manifest_does_not_depend_on_internal_crates() {
    let manifest = include_str!("../Cargo.toml");
    let sections = parse_manifest_sections(manifest);

    let mut saw_normal_dependencies = false;
    for (header, body) in &sections {
        if !header_is_normal_dependencies(header) {
            continue;
        }
        saw_normal_dependencies = true;
        for forbidden in FORBIDDEN_INTERNAL_CRATES {
            assert!(
                !body_mentions_crate(body, forbidden),
                "rmux-sdk must not declare a normal dependency on {forbidden} (section [{header}])",
            );
        }
    }

    assert!(
        saw_normal_dependencies,
        "rmux-sdk Cargo.toml must declare a [dependencies] section",
    );
}

#[test]
fn rmux_sdk_tokio_dependency_stays_narrow_async_runtime_plumbing() {
    let manifest = include_str!("../Cargo.toml");
    let sections = parse_manifest_sections(manifest);
    let dependencies_body = sections
        .iter()
        .find(|(header, _)| header == "dependencies")
        .map(|(_, body)| body.as_str())
        .expect("rmux-sdk manifest declares [dependencies]");

    let tokio = dependency_line(dependencies_body, "tokio")
        .expect("rmux-sdk transport actor depends on Tokio async I/O plumbing");

    assert!(
        tokio.contains("\"macros\""),
        "rmux-sdk production futures use tokio::select! and require the Tokio macros feature",
    );

    for forbidden_feature in ["net", "process", "rt-multi-thread", "time"] {
        assert!(
            !tokio.contains(&format!("\"{forbidden_feature}\"")),
            "rmux-sdk must not enable Tokio feature `{forbidden_feature}` as a normal dependency",
        );
    }
}

#[test]
fn rmux_sdk_cargo_manifest_keeps_internal_crates_out_of_dev_and_build_sections() {
    let manifest = include_str!("../Cargo.toml");
    let sections = parse_manifest_sections(manifest);

    for (header, body) in &sections {
        if !header_is_any_dependency_section(header) {
            continue;
        }
        for forbidden in FORBIDDEN_INTERNAL_CRATES {
            assert!(
                !body_mentions_crate(body, forbidden),
                "rmux-sdk must not pull in {forbidden} via [{header}]; SDK independence \
                 must hold for all dependency kinds, not just normal deps",
            );
        }
    }
}

#[test]
fn manifest_section_parser_is_honest_about_normal_and_dev_separation() {
    let synthetic = "[package]\nname = \"x\"\n\n\
                     [dependencies]\nrmux-proto = { path = \"../rmux-proto\" }\n\
                     # commented = \"should be ignored\"\n\
                     \n\
                     [dev-dependencies]\nrmux-core = { path = \"../rmux-core\" }\n";

    let sections = parse_manifest_sections(synthetic);
    let headers: Vec<&str> = sections.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(headers, vec!["package", "dependencies", "dev-dependencies"]);

    let dependencies_body = sections
        .iter()
        .find(|(h, _)| h == "dependencies")
        .map(|(_, body)| body.as_str())
        .expect("synthetic manifest declares [dependencies]");
    assert!(body_mentions_crate(dependencies_body, "rmux-proto"));
    assert!(!body_mentions_crate(dependencies_body, "rmux-core"));

    let dev_body = sections
        .iter()
        .find(|(h, _)| h == "dev-dependencies")
        .map(|(_, body)| body.as_str())
        .expect("synthetic manifest declares [dev-dependencies]");
    assert!(body_mentions_crate(dev_body, "rmux-core"));
    assert!(!body_mentions_crate(dev_body, "rmux-proto"));
}

#[test]
fn manifest_section_parser_treats_array_of_tables_as_non_headers() {
    let synthetic = "[package]\nname = \"x\"\n\n\
                     [[bin]]\nname = \"a\"\n\
                     [dependencies]\nrmux-proto = \"1\"\n";

    let sections = parse_manifest_sections(synthetic);
    let headers: Vec<&str> = sections.iter().map(|(h, _)| h.as_str()).collect();
    assert!(
        !headers.contains(&"bin"),
        "[[bin]] arrays of tables must not be parsed as a [bin] section header"
    );
    assert!(headers.contains(&"dependencies"));
}

#[test]
fn manifest_body_mentions_crate_ignores_substring_matches() {
    assert!(body_mentions_crate(
        "rmux-core = { path = \"../rmux-core\" }",
        "rmux-core"
    ));
    assert!(!body_mentions_crate(
        "# rmux-core notes: do not depend on it",
        "rmux-core"
    ));
    assert!(
        !body_mentions_crate(
            "rmux-core-extras = { path = \"../rmux-core-extras\" }",
            "rmux-core"
        ),
        "must not match a different crate that happens to share a prefix",
    );
}

#[test]
fn rmux_sdk_cargo_manifest_does_not_redeclare_proto_in_dev_dependencies() {
    let manifest = include_str!("../Cargo.toml");
    let sections = parse_manifest_sections(manifest);

    let in_normal = sections
        .iter()
        .any(|(header, body)| header == "dependencies" && body_mentions_crate(body, "rmux-proto"));
    let in_dev = sections.iter().any(|(header, body)| {
        header == "dev-dependencies" && body_mentions_crate(body, "rmux-proto")
    });

    assert!(
        in_normal,
        "rmux-sdk must depend on rmux-proto as a normal dep"
    );
    assert!(
        !in_dev,
        "rmux-sdk must not redeclare rmux-proto in [dev-dependencies]; the normal dep \
         is already visible in tests, and a duplicate entry misleads readers about the \
         crate's true dependency surface",
    );
}
