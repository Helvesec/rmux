use super::super::{schema, Command, ResizePaneSize};
use schema::CommandSpec;

#[test]
fn resize_required_dimensions_preserve_option_shaped_targets() {
    for (arguments, target) in [
        (vec!["-x", "20", "-t", "-y"], "-y"),
        (vec!["-x", "20", "-t", "-x"], "-x"),
        (vec!["-x20", "-t-y"], "-y"),
        (vec!["-x20", "-t-x"], "-x"),
        (vec!["-x", "20", "-t", "--"], "--"),
        (vec!["-x", "20", "-t", "ordinary"], "ordinary"),
    ] {
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
            let parsed = parsed.unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
            assert_eq!(parsed.target.as_ref().unwrap().raw(), target);
            assert_eq!(parsed.columns, Some(ResizePaneSize::Cells(20)));
            assert_eq!(parsed.rows, None);
        }
    }
}

#[test]
fn resize_required_dimensions_reject_real_missing_values() {
    for flag in ["-x", "-y"] {
        for prefix in [vec![], vec!["-t", "pane"], vec!["-t", "-x"]] {
            let mut arguments = prefix;
            arguments.push(flag);
            let error = schema::ResizePane::parse(
                arguments.iter().map(|arg| (*arg).to_owned()).collect(),
                String::new(),
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("{flag} expects an argument")),
                "{arguments:?}: {error}"
            );
        }
    }
}

#[test]
fn resize_absolute_dimensions_do_not_require_a_target() {
    let parsed = schema::ResizePane::parse(vec!["-x".into(), "20".into()], String::new())
        .expect("implicit target is valid");
    assert!(parsed.target.is_none());
    assert_eq!(parsed.columns, Some(ResizePaneSize::Cells(20)));
}
