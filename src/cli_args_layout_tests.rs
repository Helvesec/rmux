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
fn select_layout_rejects_old_layout_flag_with_layout_argument() {
    let error = parse_args(&["select-layout", "-o", "tiled"]).unwrap_err();

    assert!(error.to_string().contains("too many arguments"));
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
