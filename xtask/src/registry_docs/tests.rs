use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

mod references;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn fixture() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target")
        .join("tmp");
    let path = root.join(format!(
        "registry-docs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn command_requires_one_destination_and_accepts_one_read_only_mode() {
    assert!(
        Options::parse(["--output-dir", "fixture", "--check"].map(str::to_owned))
            .unwrap()
            .check
    );
    for args in [
        vec![],
        vec!["--check"],
        vec!["--output-dir"],
        vec!["--output-dir", "--check"],
        vec!["--output-dir", "a", "--output-dir", "b"],
        vec!["--output-dir", "a", "--check", "--check"],
    ] {
        assert!(Options::parse(args.into_iter().map(str::to_owned)).is_err());
    }
}

#[test]
fn generator_preserves_unowned_documents_and_checker_does_not_write() {
    let output = fixture();
    let artifacts = [
        Artifact {
            name: "first",
            content: "owned: generated\n".into(),
            marker: "owned:",
        },
        Artifact {
            name: "human",
            content: "owned: generated\n".into(),
            marker: "owned:",
        },
    ];
    fs::write(output.join("human"), "human document").unwrap();
    assert!(publish(&output, false, &artifacts).is_err());
    assert!(!output.join("first").exists());
    assert_eq!(
        fs::read_to_string(output.join("human")).unwrap(),
        "human document"
    );
    assert!(publish(&output, true, &artifacts).is_err());
    assert!(!output.join("first").exists());
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn option_projection_uses_the_complete_existing_registry() {
    // Test the shared source inclusion, including named-only and array options.
    let entries = crate::option_registry::registry();
    assert!(entries.iter().any(|entry| entry.name() == "status-right"));
    assert!(entries.iter().any(|entry| entry.wire_option().is_none()));
    assert!(entries.iter().any(|entry| entry.is_array()));
    let projected = snapshot::options();
    assert_eq!(projected.len(), entries.len());
    for (entry, projection) in entries.iter().zip(projected) {
        assert_eq!(projection.name, entry.name());
        assert_eq!(projection.aliases, entry.aliases());
        assert_eq!(projection.array, entry.is_array());
        assert_eq!(projection.separator, entry.separator());
        let defaults = match entry.default_value() {
            crate::option_registry::DefaultValue::Scalar(value) => vec![value],
            crate::option_registry::DefaultValue::Array(values) => values.to_vec(),
        };
        assert!(defaults
            .iter()
            .all(|value| projection.defaults.contains(value)));
        assert_eq!(
            projection.defaults.len(),
            projection.default_platforms.len()
        );
        let scopes: Vec<_> = [(1, "server"), (2, "session"), (4, "window"), (8, "pane")]
            .into_iter()
            .filter_map(|(mask, name)| (entry.scope_mask() & mask != 0).then_some(name))
            .collect();
        assert_eq!(projection.scopes, scopes);
    }
}

#[test]
fn report_preserves_authority_and_renderer_preserves_literal_defaults() {
    let ledger_text = include_str!("../../../tests/reference/tmux_compat/divergences.toml");
    let source: toml::Value = toml::from_str(ledger_text).unwrap();
    let ledger = ledger::parse(ledger_text).unwrap();
    let snapshot = snapshot::Snapshot {
        version: "fixture",
        defaults_platform: "fixture",
        top_level_help: String::new(),
        commands: vec![],
        cli_aliases: vec![],
        local_commands: vec![],
        options: snapshot::options(),
    };
    let artifacts = render::artifacts(&snapshot, &ledger).unwrap();
    let json: serde_json::Value = serde_json::from_str(&artifacts[4].content).unwrap();
    let entries = source["entry"].as_array().unwrap();
    assert_eq!(
        json["divergences"]["entry"].as_array().unwrap().len(),
        entries.len()
    );
    for (expected, actual) in entries
        .iter()
        .zip(json["divergences"]["entry"].as_array().unwrap())
    {
        for (key, value) in expected.as_table().unwrap() {
            assert_eq!(actual[key], serde_json::to_value(value).unwrap(), "{key}");
        }
    }
    for option in &snapshot.options {
        assert!(artifacts[1]
            .content
            .contains(&format!("## `{}`", option.name)));
        for value in &option.defaults {
            assert!(
                artifacts[1].content.contains(value),
                "literal default {}",
                option.name
            );
        }
    }
}

#[test]
fn ledger_rejects_duplicate_ids_and_preserves_limitations() {
    let text = "schema = 1\n[policy]\noracle = 'tmux 3.7b'\nunlisted_divergences_are_bugs = true\n[[entry]]\nid = 'C-D1'\nsummary = 'registered'\ninventory = 'bounded'\nevidence = ['tests/probe.rs']\nlimitations = ['pending']\n";
    let ledger = ledger::parse(text).unwrap();
    assert_eq!(ledger.entry[0].limitations, ["pending"]);
    assert!(ledger::parse(
        &(text.to_owned()
            + "[[entry]]\nid = 'C-D1'\nsummary = 'duplicate'\ninventory = 'bounded'\n")
    )
    .is_err());
}

#[test]
fn long_only_flags_without_runtime_signatures_are_refused() {
    let command = clap::Command::new("list-commands").arg(
        clap::Arg::new("ghost")
            .long("ghost")
            .action(clap::ArgAction::SetTrue),
    );
    let runtime = clap::Command::new("list-commands");
    assert!(snapshot::validate_flags(&command, &runtime)
        .unwrap_err()
        .to_string()
        .contains("list-commands --ghost"));
}

#[test]
fn generated_man_preserves_short_help_notes() {
    let snapshot = snapshot::Snapshot {
        version: "fixture",
        defaults_platform: "fixture",
        top_level_help: String::new(),
        commands: vec![],
        cli_aliases: vec![],
        local_commands: vec![],
        options: vec![],
    };
    let ledger = ledger::parse("schema = 1\n[policy]\noracle = 'tmux 3.7b'\nunlisted_divergences_are_bugs = true\n[[entry]]\nid = 'C-D1'\nsummary = 'fixture'\ninventory = 'bounded'\nevidence = ['probe.rs']\n").unwrap();
    let artifacts = render::artifacts(&snapshot, &ledger).unwrap();
    let man = &artifacts[2].content;
    assert!(man.contains(".SH NOTES\n"));
    assert!(man.contains(".B -Vh\n"));
    assert!(man.contains(".BR \"rmux <command> --help\" ."));
    assert!(man.contains(".BR \"rmux split-window -h\" ."));
}

#[test]
fn undocumented_and_wrong_arity_flags_are_refused() {
    let runtime = clap::Command::new("list-commands");
    let command = clap::Command::new("list-commands").arg(
        clap::Arg::new("ignored")
            .short('z')
            .action(clap::ArgAction::SetTrue),
    );
    assert!(snapshot::validate_flags(&command, &runtime)
        .unwrap_err()
        .to_string()
        .contains("list-commands -z"));
    let command = clap::Command::new("list-commands").arg(
        clap::Arg::new("format")
            .short('F')
            .action(clap::ArgAction::SetTrue),
    );
    assert!(snapshot::validate_flags(&command, &runtime).is_err());
    let command = clap::Command::new("list-commands").arg(clap::Arg::new("format").short('F'));
    assert!(snapshot::validate_flags(&command, &runtime).is_ok());
}

#[test]
fn long_runtime_flags_keep_exact_names_actions_and_arity() {
    let runtime = clap::Command::new("capabilities")
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("help")
                .long("help")
                .action(clap::ArgAction::Help),
        );
    assert!(snapshot::validate_flags(&runtime, &runtime).is_ok());
    for arg in [
        clap::Arg::new("ghost")
            .long("ghost")
            .action(clap::ArgAction::SetTrue),
        clap::Arg::new("json-extra")
            .long("json-extra")
            .action(clap::ArgAction::SetTrue),
        clap::Arg::new("json").long("json"),
        clap::Arg::new("json")
            .long("json")
            .short('j')
            .action(clap::ArgAction::SetTrue),
        clap::Arg::new("help")
            .long("help")
            .action(clap::ArgAction::SetTrue),
    ] {
        assert!(
            snapshot::validate_flags(&clap::Command::new("capabilities").arg(arg), &runtime)
                .is_err()
        );
    }
}

#[test]
fn ledger_refuses_missing_and_escaping_evidence() {
    let output = fixture();
    let text = "schema = 1\n[policy]\noracle = 'tmux 3.7b'\nunlisted_divergences_are_bugs = true\n[[entry]]\nid = 'C-D1'\nsummary = 'registered'\ninventory = 'bounded'\nevidence = ['probe.rs']\n";
    let ledger = ledger::parse(text).unwrap();
    assert!(ledger.validate_references(&output).is_err());
    fs::write(output.join("probe.rs"), "fn probe() {}").unwrap();
    assert!(ledger.validate_references(&output).is_ok());
    assert!(ledger::parse(&text.replace("probe.rs", "../probe.rs"))
        .unwrap()
        .validate_references(&output)
        .is_err());
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn ledger_rejects_inconsistent_policy_and_unreferenced_divergences() {
    let text = "schema = 1\n[policy]\noracle = 'tmux 3.7b'\nunlisted_divergences_are_bugs = true\n[[entry]]\nid = 'C-D1'\nsummary = 'registered'\ninventory = 'bounded'\nevidence = ['tests/probe.rs']\n";
    assert!(ledger::parse(text).is_ok());
    for mutation in [
        text.replace("tmux 3.7b", "tmux 3.4"),
        text.replace("= true", "= false"),
        text.replace("C-D1", "C-D01"),
        text.replace("evidence = ['tests/probe.rs']", ""),
    ] {
        assert!(ledger::parse(&mutation).is_err(), "{mutation}");
    }
}

#[test]
fn generator_keeps_central_manual_version_and_experimental_kitty_warning() {
    let snapshot = snapshot::Snapshot {
        version: env!("CARGO_PKG_VERSION"),
        defaults_platform: "fixture",
        top_level_help: String::new(),
        commands: vec![],
        cli_aliases: vec![],
        local_commands: vec![],
        options: vec![],
    };
    let ledger = ledger::parse(include_str!(
        "../../../tests/reference/tmux_compat/divergences.toml"
    ))
    .unwrap();
    let artifacts = render::artifacts(&snapshot, &ledger).unwrap();
    let manual = &artifacts[2].content;
    let checked_in = include_str!("../../../docs/man/rmux.1");
    let header = ".TH RMUX 1 \"\" \"RMUX 0.11.0\" \"RMUX Registry Reference\"";
    let warning = ".SH EXPERIMENTAL KITTY KEYBOARD\n\
Kitty Keyboard is disabled by default and full protocol support is not qualified.\n\
The default build ignores its negotiation and retains legacy and xterm extended keys.\n\
Kitty graphics is independent. To opt in, build with\n\
.B cargo build --release --features kitty-keyboard\n\
and restart the daemon using that binary. Applications may then negotiate enhancement flags.\n";
    assert_eq!(snapshot.version, "0.11.0");
    assert_eq!(checked_in.lines().nth(1), Some(header));
    assert_eq!(manual.lines().nth(1), Some(header));
    assert!(checked_in.ends_with(warning));
    assert!(
        manual.ends_with(warning),
        "generator dropped central Kitty warning"
    );
    let (_, explanations) = checked_in
        .split_once(".SH DESCRIPTION\n")
        .expect("central manual explanations");
    let (explanations, _) = explanations
        .split_once(".SH EXPERIMENTAL KITTY KEYBOARD\n")
        .expect("end of central manual explanations");
    assert!(
        manual.contains(explanations),
        "generator dropped central runtime guidance, including graphics and web sharing"
    );
}
