use super::parse;

fn parse_args(args: &[&str]) -> Result<super::Cli, clap::Error> {
    let mut full_args = vec!["rmux"];
    full_args.extend_from_slice(args);
    parse(full_args)
}

#[test]
fn select_layout_accepts_all_standard_layout_names() {
    for layout_name in [
        "main-vertical",
        "main-horizontal",
        "even-horizontal",
        "even-vertical",
        "tiled",
    ] {
        let cli = parse_args(&["select-layout", "-t", "alpha:0", layout_name]).unwrap();

        match cli.command.expect("parsed command") {
            super::Command::SelectLayout(args) => {
                assert_eq!(args.layout.as_deref(), Some(layout_name));
            }
            _ => panic!("expected SelectLayout command"),
        }
    }
}

#[test]
fn select_layout_accepts_old_layout_flag() {
    let cli = parse_args(&["select-layout", "-o", "-t", "alpha:0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::Command::SelectLayout(args) => {
            assert!(args.old);
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:0");
            assert!(args.layout.is_none());
        }
        _ => panic!("expected SelectLayout command"),
    }
}

#[test]
fn select_layout_accepts_tmux_mode_clusters_and_preserves_all_flags() {
    for arguments in [
        &["select-layout", "-En", "-t", "alpha:0"][..],
        &["select-layout", "-nE", "-t", "alpha:0"][..],
        &["select-layout", "-E", "-n", "-t", "alpha:0"][..],
        &["select-layout", "-Enop", "-t", "alpha:0", "tiled"][..],
    ] {
        let cli = parse_args(arguments)
            .unwrap_or_else(|error| panic!("{arguments:?} should parse like tmux 3.7b: {error}"));

        match cli.command.expect("parsed command") {
            super::Command::SelectLayout(args) => {
                assert!(args.next, "-n must survive parsing for {arguments:?}");
                assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:0");
                if arguments.contains(&"-Enop") {
                    assert!(args.spread);
                    assert!(args.old);
                    assert!(args.previous);
                    assert_eq!(args.layout.as_deref(), Some("tiled"));
                }
            }
            _ => panic!("expected SelectLayout command"),
        }
    }
}

#[test]
fn select_layout_old_mode_defers_its_optional_operand_to_runtime() {
    let cli = parse_args(&["select-layout", "-o", "tiled"])
        .expect("tmux accepts one operand with -o and validates it as a custom layout");

    match cli.command.expect("parsed command") {
        super::Command::SelectLayout(args) => {
            assert!(args.old);
            assert_eq!(args.layout.as_deref(), Some("tiled"));
        }
        _ => panic!("expected SelectLayout command"),
    }
}

#[test]
fn next_layout_accepts_window_targets() {
    let cli = parse_args(&["next-layout", "-t", "alpha:3"]).unwrap();

    match cli.command.expect("parsed command") {
        super::Command::NextLayout(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:3")
        }
        _ => panic!("expected NextLayout command"),
    }
}

#[test]
fn previous_layout_preserves_session_targets_for_runtime_resolution() {
    let cli = parse_args(&["previous-layout", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::Command::PreviousLayout(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha")
        }
        _ => panic!("expected PreviousLayout command"),
    }
}

#[test]
fn window_snapshot_commands_parse_as_explicit_rmux_commands() {
    let cli = parse_args(&["export-window-snapshot", "-t", "alpha:3"]).unwrap();
    match cli.command.expect("parsed command") {
        super::Command::ExportWindowSnapshot(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:3");
        }
        other => panic!("expected ExportWindowSnapshot command, got {other:?}"),
    }

    let cli = parse_args(&["restore-window-snapshot", "-t", "alpha:3", "snapshot.json"]).unwrap();
    match cli.command.expect("parsed command") {
        super::Command::RestoreWindowSnapshot(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:3");
            assert_eq!(
                args.snapshot_path,
                std::path::PathBuf::from("snapshot.json")
            );
        }
        other => panic!("expected RestoreWindowSnapshot command, got {other:?}"),
    }
}

#[test]
fn select_layout_still_treats_snapshot_json_as_legacy_layout_operand() {
    let json = r#"{"version":1,"floating":[]}"#;
    let cli = parse_args(&["select-layout", "-t", "alpha:0", json]).unwrap();

    match cli.command.expect("parsed command") {
        super::Command::SelectLayout(args) => {
            assert_eq!(args.layout.as_deref(), Some(json));
        }
        other => panic!("expected SelectLayout command, got {other:?}"),
    }
}
