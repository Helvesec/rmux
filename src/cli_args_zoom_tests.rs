use super::{parse, Command};

fn parse_args(args: &[&str]) -> Result<super::Cli, clap::Error> {
    let mut full_args = vec!["rmux"];
    full_args.extend_from_slice(args);
    parse(full_args)
}

fn target_text(target: &Option<super::TargetSpec>) -> String {
    target.as_ref().expect("target").to_string()
}

#[test]
fn resize_pane_accepts_zoom_without_columns() {
    let cli = parse_args(&["resize-pane", "-t", "alpha:0.1", "-Z"]).unwrap();

    match cli.command.expect("parsed command") {
        Command::ResizePane(args) => {
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.1"
            );
            assert!(args.zoom);
            assert_eq!(args.columns, None);
        }
        _ => panic!("expected ResizePane command"),
    }
}

#[test]
fn resize_pane_zoom_after_columns_follows_tmux_last_wins() {
    let cli = parse_args(&["resize-pane", "-t", "alpha:0.1", "-x", "34", "-Z"]).unwrap();

    match cli.command.expect("parsed command") {
        Command::ResizePane(args) => {
            assert_eq!(target_text(&args.target), "alpha:0.1");
            assert!(args.zoom);
            assert_eq!(args.columns, None);
        }
        _ => panic!("expected ResizePane command"),
    }
}

#[test]
fn display_panes_accepts_session_targets() {
    let cli = parse_args(&["display-panes", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        Command::DisplayPanes(args) => assert_eq!(target_text(&args.target), "alpha"),
        _ => panic!("expected DisplayPanes command"),
    }
}
