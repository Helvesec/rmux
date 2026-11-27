use super::*;

#[test]
fn fmp1_direct_cli_parses_signed_axes_and_rejects_mixed_actions() {
    for (arguments, expected) in [
        (
            &["resize-pane", "-t", "%7", "-X", "-4"][..],
            (Some(-4), None),
        ),
        (
            &["resizep", "-Y2147483647", "-X+0", "-t", "%7"][..],
            (Some(0), Some(i32::MAX)),
        ),
    ] {
        let parsed = parse_args(arguments).unwrap();
        let super::super::Command::ResizePane(args) = parsed.command.unwrap() else {
            panic!("expected resize-pane")
        };
        assert_eq!((args.position_x, args.position_y), expected);
        assert_eq!(target_text(&args.target), "%7");
    }

    for arguments in [
        &["resize-pane", "-X", "1", "-x", "20"][..],
        &["resize-pane", "-Y", "1", "-R"][..],
        &["resize-pane", "-X", "1", "-M"][..],
        &["resize-pane", "-X", "1", "-T"][..],
        &["resize-pane", "-X", "1", "-Z"][..],
        &["resize-pane", "-X", "1", "-X", "2"][..],
        &["resize-pane", "-Y", "1%"][..],
        &["resize-pane", "-X", "2147483648"][..],
        &["resize-pane", "-X", "1", "2"][..],
    ] {
        assert!(parse_args(arguments).is_err(), "{arguments:?}");
    }

    let legacy = parse_args(&["resize-pane", "-x", "20", "-R", "2"]).unwrap();
    let super::super::Command::ResizePane(legacy) = legacy.command.unwrap() else {
        panic!("expected legacy resize-pane")
    };
    assert!(legacy.position_x.is_none() && legacy.position_y.is_none());
    assert!(legacy.columns.is_some());
    assert_eq!(legacy.right, Some(2));
}
