use super::super::{schema, Command, ResizePaneSize};
use schema::CommandSpec;

#[test]
fn resize_required_dimensions_schema_help_keeps_target_values() {
    for target in ["-x", "-y", "--"] {
        let parsed = schema::ResizePane::help()
            .try_get_matches_from(["-x", "20", "-t", target])
            .expect("required target value stays opaque");
        assert_eq!(
            parsed
                .get_one::<super::super::TargetSpec>("target")
                .unwrap()
                .raw(),
            target
        );
    }
}

#[test]
fn resize_required_dimensions_alias_and_parser_boundaries() {
    let parsed = super::parse_args(&["resizep", "-x", "20", "-t", "-y"])
        .expect("resizep shares the corrected parser");
    let Command::ResizePane(parsed) = parsed.command.unwrap() else {
        panic!("expected resize-pane");
    };
    assert_eq!(parsed.target.unwrap().raw(), "-y");
    assert_eq!(parsed.columns, Some(ResizePaneSize::Cells(20)));

    for arguments in [vec!["--", "-x"], vec!["3", "-y"], vec!["-", "-x"]] {
        let typed = schema::ResizePane::parse(
            arguments.iter().map(|arg| (*arg).to_owned()).collect(),
            String::new(),
        );
        let mut argv = vec!["resize-pane"];
        argv.extend_from_slice(&arguments);
        let full = super::parse_args(&argv).map(|cli| match cli.command.unwrap() {
            Command::ResizePane(args) => args,
            _ => panic!("expected resize-pane"),
        });
        for parsed in [typed, full] {
            let error = parsed.unwrap_err().to_string();
            assert!(
                !error.contains("expects an argument"),
                "{arguments:?}: {error}"
            );
        }
    }
}

#[test]
fn resize_required_dimensions_preserve_neighbor_validation() {
    for (arguments, expected) in [
        (vec!["-x", "-1", "-t", "-y"], "width too small"),
        (vec!["-x", "bad", "-t", "-y"], "width invalid"),
        (vec!["-y", "2147483648", "-t", "-x"], "height too large"),
        (vec!["-Z", "-x"], "-x expects an argument"),
        (vec!["-T", "-y"], "-y expects an argument"),
    ] {
        let error = schema::ResizePane::parse(
            arguments.iter().map(|arg| (*arg).to_owned()).collect(),
            String::new(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "{arguments:?}: {error}"
        );
    }
    let parsed = schema::ResizePane::parse(
        ["-x", "20", "-y", "bad", "-t", "-x"]
            .map(str::to_owned)
            .to_vec(),
        String::new(),
    )
    .expect("height validation remains deferred after a valid width");
    assert_eq!(
        parsed.rows,
        Some(ResizePaneSize::DeferredError("height invalid"))
    );
    assert_eq!(parsed.target.unwrap().raw(), "-x");
}

#[test]
fn s2528_resize_window_full_and_typed_parsers_preserve_range_errors() {
    for command in ["resize-window", "resizew"] {
        for (flag, label) in [("-x", "width"), ("-y", "height")] {
            for value in ["65536", "70000", "2147483648", "9223372036854775808"] {
                let full = super::parse_args(&[command, flag, value]).unwrap_err();
                let typed = schema::ResizeWindow::parse(
                    [flag, value].map(str::to_owned).to_vec(),
                    String::new(),
                )
                .unwrap_err();
                for error in [full, typed] {
                    assert!(
                        error.to_string().contains(&format!("{label} too large")),
                        "{error}"
                    );
                }
            }
        }
    }
}
