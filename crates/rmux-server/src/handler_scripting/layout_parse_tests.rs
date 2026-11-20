use super::*;

fn token(value: &str) -> String {
    value.to_owned()
}

fn fixture() -> (ManagedSessionStore, TargetFindContext) {
    let session_name = rmux_proto::SessionName::new("alpha").expect("valid session name");
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("session creation succeeds");
    let find_context = TargetFindContext::from_target(rmux_proto::Target::Window(
        WindowTarget::with_window(session_name, 0),
    ));
    (sessions, find_context)
}

fn parse(arguments: &[&str]) -> Result<ParsedSelectLayout, RmuxError> {
    let (sessions, find_context) = fixture();
    parse_select_layout(
        CommandTokens::new(arguments.iter().map(|argument| token(argument)).collect()),
        &sessions,
        &find_context,
    )
}

#[test]
fn select_layout_rejects_unknown_option_shapes_before_layout() {
    for (arguments, expected) in [
        (&["-x"][..], "command select-layout: unknown flag -x"),
        (&["--bogus"][..], "command select-layout: invalid flag --"),
        (&["-nx"][..], "command select-layout: unknown flag -x"),
        (&["-value"][..], "command select-layout: unknown flag -v"),
    ] {
        assert_eq!(
            parse(arguments).expect_err("unknown option must fail before layout parsing"),
            RmuxError::Server(expected.to_owned())
        );
    }
}

#[test]
fn select_layout_preserves_legitimate_flags_and_separator() {
    for arguments in [
        &["-E", "-t", "alpha:0"][..],
        &["-n", "-t", "alpha:0"][..],
        &["-o", "-t", "alpha:0"][..],
        &["-p", "-t", "alpha:0"][..],
        &["-t", "alpha:0", "tiled"][..],
        &["-ntalpha:0"][..],
        &["--", "tiled"][..],
    ] {
        parse(arguments).unwrap_or_else(|error| {
            panic!("select-layout rejected legitimate arguments {arguments:?}: {error}")
        });
    }
}

#[test]
fn select_layout_mode_clusters_use_tmux_priority_independent_of_spelling() {
    for arguments in [
        &["-En", "-t", "alpha:0"][..],
        &["-nE", "-t", "alpha:0"][..],
        &["-E", "-n", "-t", "alpha:0"][..],
        &["-Enop", "-t", "alpha:0"][..],
    ] {
        assert!(
            matches!(
                parse(arguments),
                Ok(ParsedSelectLayout::Request(Request::NextLayout(_)))
            ),
            "-n must govern {arguments:?}"
        );
    }

    for arguments in [&["-Ep", "-t", "alpha:0"][..], &["-op", "-t", "alpha:0"][..]] {
        assert!(
            matches!(
                parse(arguments),
                Ok(ParsedSelectLayout::Request(Request::PreviousLayout(_)))
            ),
            "-p must govern {arguments:?}"
        );
    }

    assert!(matches!(
        parse(&["-Eo", "-t", "alpha:0"]),
        Ok(ParsedSelectLayout::Request(Request::SpreadLayout(_)))
    ));
    assert!(matches!(
        parse(&["-o", "-t", "alpha:0"]),
        Ok(ParsedSelectLayout::Request(Request::SelectOldLayout(_)))
    ));
}

#[test]
fn select_layout_mode_operand_semantics_match_tmux() {
    for arguments in [
        &["-n", "-t", "alpha:0", "tiled"][..],
        &["-p", "-t", "alpha:0", "tiled"][..],
        &["-E", "-t", "alpha:0", "tiled"][..],
    ] {
        parse(arguments).unwrap_or_else(|error| {
            panic!("governing navigation/spread mode must ignore one operand: {error}")
        });
    }

    let ParsedSelectLayout::Request(Request::SelectCustomLayout(request)) =
        parse(&["-o", "-t", "alpha:0", "tiled"]).expect("-o accepts one custom-layout operand")
    else {
        panic!("-o with an operand must use strict custom-layout parsing at runtime");
    };
    assert_eq!(request.layout, "tiled");

    assert!(
        parse(&["-En", "-t", "alpha:0", "tiled", "extra"]).is_err(),
        "a governing mode must still reject a second operand"
    );
}

#[test]
fn select_layout_preserves_missing_target_error() {
    assert_eq!(
        parse(&["-t"]).expect_err("-t without a target must fail"),
        RmuxError::Server("missing -t target".to_owned())
    );
}

#[test]
fn l1_040_resize_modes_precede_deferred_height_and_preserve_target() {
    for width in ["10", "25%"] {
        for (flags, mode) in [
            (&["-Z"][..], ResizePaneAdjustment::Zoom),
            (&["-T"][..], ResizePaneAdjustment::TrimBelow),
            (&["-Z", "-T"][..], ResizePaneAdjustment::TrimBelow),
            (&["-T", "-Z"][..], ResizePaneAdjustment::TrimBelow),
        ] {
            let (sessions, context) = fixture();
            let mut argv = vec!["-t", "alpha:0.0", "-x", width, "-y", "bad"];
            argv.extend_from_slice(flags);
            let request = parse_resize_pane(
                CommandTokens::new(argv.into_iter().map(token).collect()),
                &sessions,
                &context,
            )
            .unwrap();
            let Request::ResizePane(request) = request else {
                panic!("mode must precede deferred height")
            };
            assert_eq!(
                request.target,
                rmux_proto::PaneTarget::with_window(
                    rmux_proto::SessionName::new("alpha").unwrap(),
                    0,
                    0
                )
            );
            assert_eq!(request.adjustment, mode);
        }
    }
}

#[test]
fn l1_040_no_mode_deferred_height_diagnostic_matches_native_oracle() {
    // Keep this measured expectation even when the parser reports a divergence.
    // Collect both numeric and percent requests before comparing diagnostics.
    let mut errors = Vec::new();
    for width in ["10", "25%"] {
        let (sessions, context) = fixture();
        let argv = ["-t", "alpha:0.0", "-x", width, "-y", "bad"];
        let request = parse_resize_pane(
            CommandTokens::new(argv.into_iter().map(token).collect()),
            &sessions,
            &context,
        )
        .unwrap();
        let Request::ResizePaneAxes(request) = request else {
            panic!("sequential width/error request")
        };
        assert_eq!(
            request.target,
            rmux_proto::PaneTarget::with_window(
                rmux_proto::SessionName::new("alpha").unwrap(),
                0,
                0
            )
        );
        assert_eq!(request.columns, Some(if width == "10" { 10 } else { 20 }));
        errors.push(request.rows);
    }
    assert_eq!(
        errors,
        vec![Err(RmuxError::Server("height invalid".into())); 2]
    );
}

#[test]
fn l1_041_height_diagnostic_preserves_other_error_classes() {
    for (flag, value, expected) in [
        (
            "-x",
            "bad",
            "resize-pane -x expects an integer cell count: invalid digit found in string",
        ),
        (
            "-y",
            "-1",
            "resize-pane -y expects a non-negative cell count",
        ),
        ("-y", "2147483648", "resize-pane -y cell count is too large"),
        (
            "-y",
            "9223372036854775808",
            "resize-pane -y expects an integer cell count: number too large to fit in target type",
        ),
    ] {
        let (sessions, context) = fixture();
        let request = parse_resize_pane(
            CommandTokens::new(
                ["-t", "alpha:0.0", "-x", "10", flag, value]
                    .into_iter()
                    .map(token)
                    .collect(),
            ),
            &sessions,
            &context,
        );
        let error = if flag == "-x" {
            request.unwrap_err()
        } else {
            let Request::ResizePaneAxes(request) = request.unwrap() else {
                panic!("height error remains deferred")
            };
            assert_eq!(request.columns, Some(10));
            request.rows.unwrap_err()
        };
        assert_eq!(error, RmuxError::Server(expected.into()));
    }

    // Percentage parsing has a separate diagnostic contract, unchanged here.
    let (sessions, context) = fixture();
    let expected = RmuxError::Server(
        "resize-pane -y expects a percentage: invalid digit found in string".into(),
    );
    let Request::ResizePaneAxes(request) = parse_resize_pane(
        CommandTokens::new(
            ["-t", "alpha:0.0", "-x", "10", "-y", "bad%"]
                .into_iter()
                .map(token)
                .collect(),
        ),
        &sessions,
        &context,
    )
    .unwrap() else {
        panic!("percentage error remains deferred")
    };
    assert_eq!(request.columns, Some(10));
    assert_eq!(request.rows, Err(expected));
}

#[test]
fn l1_041_height_error_does_not_bypass_structure_or_target_resolution() {
    for (argv, expected) in [
        (
            &["-t", "alpha:0.0", "-x", "10", "-y"][..],
            "missing -y value",
        ),
        (
            &["-t", "alpha:0.0", "-x", "10", "-y", "bad", "-Q"][..],
            "command resize-pane: unknown flag -Q",
        ),
        (
            &["-t", "alpha:0.9", "-x", "25%", "-y", "bad"][..],
            "resize-pane could not resolve dimensions for pane alpha:0.9",
        ),
    ] {
        let (sessions, context) = fixture();
        assert_eq!(
            parse_resize_pane(
                CommandTokens::new(argv.iter().map(|arg| token(arg)).collect()),
                &sessions,
                &context,
            )
            .unwrap_err(),
            RmuxError::Server(expected.into()),
        );
    }
}

#[test]
fn l1_041_later_valid_height_clears_deferred_error() {
    let (sessions, context) = fixture();
    let Request::ResizePane(request) = parse_resize_pane(
        CommandTokens::new(
            ["-t", "alpha:0.0", "-x", "10", "-y", "bad", "-y", "6"]
                .into_iter()
                .map(token)
                .collect(),
        ),
        &sessions,
        &context,
    )
    .unwrap() else {
        panic!("a later valid height clears the deferred error")
    };
    assert_eq!(
        request.adjustment,
        ResizePaneAdjustment::AbsoluteSize {
            columns: 10,
            rows: 6
        }
    );
}
