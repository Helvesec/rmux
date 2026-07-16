use super::{
    HookName, LayoutName, OptionName, OptionScopeSelector, PaneTarget, ResizePaneAdjustment,
    ScopeSelector, SessionName, Target, WindowTarget,
};
use crate::{
    decode_frame, encode_frame, ProcessCommand, Request, RmuxError, SelectLayoutRequest,
    SelectLayoutTarget,
};

#[test]
fn session_name_preserves_case_without_rewriting() {
    let session_name = SessionName::new("MiXeD-Case_01").expect("valid session name");
    assert_eq!(session_name.as_str(), "MiXeD-Case_01");
}

#[test]
fn legacy_command_vectors_preserve_argv_without_runtime_context() {
    let command = vec![
        "bash".to_owned(),
        "-lc".to_owned(),
        "printf '%s\\n' hello world".to_owned(),
    ];

    assert_eq!(
        ProcessCommand::from_legacy_command(Some(&command)),
        Some(ProcessCommand::Argv(command))
    );
}

#[test]
fn legacy_single_string_still_uses_shell_mode() {
    let command = vec!["printf '%s\\n' hello world".to_owned()];

    assert_eq!(
        ProcessCommand::from_legacy_command(Some(&command)),
        Some(ProcessCommand::Shell(
            "printf '%s\\n' hello world".to_owned()
        ))
    );
}

#[test]
fn session_name_rejects_empty_values() {
    assert_eq!(SessionName::new(""), Err(RmuxError::EmptySessionName));
}

#[test]
fn session_name_rewrites_colons() {
    assert_eq!(
        SessionName::new("alpha:beta").expect("rewritten session name"),
        SessionName::new("alpha_beta").expect("valid session name")
    );
}

#[test]
fn session_name_rewrites_dots() {
    assert_eq!(
        SessionName::new("alpha.beta").expect("rewritten session name"),
        SessionName::new("alpha_beta").expect("valid session name")
    );
}

#[test]
fn target_parses_session_form() {
    assert_eq!(
        Target::parse("alpha").expect("session target"),
        Target::Session(SessionName::new("alpha").expect("valid session"))
    );
}

#[test]
fn target_parses_window_form() {
    assert_eq!(
        Target::parse("alpha:0").expect("window target"),
        Target::Window(WindowTarget::new(
            SessionName::new("alpha").expect("valid session")
        ))
    );
}

#[test]
fn target_parses_pane_form() {
    assert_eq!(
        Target::parse("alpha:0.2").expect("pane target"),
        Target::Pane(PaneTarget::new(
            SessionName::new("alpha").expect("valid session"),
            2
        ))
    );
}

#[test]
fn target_parses_non_zero_window_form() {
    assert_eq!(
        Target::parse("alpha:5").expect("window target"),
        Target::Window(WindowTarget::with_window(
            SessionName::new("alpha").expect("valid session"),
            5
        ))
    );
}

#[test]
fn target_rejects_extra_segments() {
    assert_eq!(
        Target::parse("alpha:0.1.2"),
        Err(RmuxError::invalid_target(
            "alpha:0.1.2",
            "pane index must be an unsigned integer"
        ))
    );
}

#[test]
fn target_rejects_empty_session_before_colon() {
    assert_eq!(Target::parse(":0"), Err(RmuxError::EmptySessionName));
}

#[test]
fn target_rejects_trailing_colon_with_empty_tail() {
    assert!(Target::parse("alpha:").is_err());
}

#[test]
fn target_rejects_trailing_dot_after_window() {
    assert!(Target::parse("alpha:0.").is_err());
}

#[test]
fn target_parses_non_zero_window_in_pane_form() {
    assert_eq!(
        Target::parse("alpha:5.2").expect("pane target"),
        Target::Pane(PaneTarget::with_window(
            SessionName::new("alpha").expect("valid session"),
            5,
            2
        ))
    );
}

#[test]
fn target_display_round_trips_session() {
    let target = Target::parse("alpha").expect("valid");
    assert_eq!(
        Target::parse(&target.to_string()).expect("round-trip"),
        target
    );
}

#[test]
fn target_display_round_trips_window() {
    let target = Target::parse("alpha:0").expect("valid");
    assert_eq!(
        Target::parse(&target.to_string()).expect("round-trip"),
        target
    );
}

#[test]
fn target_display_round_trips_pane() {
    let target = Target::parse("alpha:0.3").expect("valid");
    assert_eq!(
        Target::parse(&target.to_string()).expect("round-trip"),
        target
    );
}

#[test]
fn target_display_round_trips_non_zero_window_pane() {
    let target = Target::parse("alpha:7.3").expect("valid");
    assert_eq!(
        Target::parse(&target.to_string()).expect("round-trip"),
        target
    );
}

#[test]
fn session_name_display_matches_inner_value() {
    let name = SessionName::new("MySession").expect("valid");
    assert_eq!(name.to_string(), "MySession");
}

#[test]
fn session_name_from_str_works() {
    let name: SessionName = "test".parse().expect("valid");
    assert_eq!(name.as_str(), "test");
}

#[test]
fn session_name_deserialize_sanitizes_invalid_wire_values() {
    let empty_payload = bincode::serialize("").expect("string encodes");
    assert!(bincode::deserialize::<SessionName>(&empty_payload).is_err());

    let payload = bincode::serialize("alpha.beta").expect("string encodes");
    assert_eq!(
        bincode::deserialize::<SessionName>(&payload).expect("sanitized session name"),
        SessionName::new("alpha_beta").expect("valid session name")
    );
}

#[test]
fn session_name_rewrites_only_special_chars() {
    assert_eq!(
        SessionName::new(":").expect("rewritten"),
        SessionName::new("_").unwrap()
    );
    assert_eq!(
        SessionName::new(".").expect("rewritten"),
        SessionName::new("_").unwrap()
    );
    assert_eq!(
        SessionName::new(":.").expect("rewritten"),
        SessionName::new("__").unwrap()
    );
}

#[test]
fn session_name_vis_encodes_controls_and_preserves_unicode() {
    let name = SessionName::new(String::from_utf8_lossy(b"a\x01\x7f\xc3\xa9").into_owned())
        .expect("rewritten");
    assert_eq!(name.as_str(), "a\\001\\177é");
}

#[test]
fn session_name_unicode_round_trips_through_serde() {
    let name = SessionName::new("café-日本").expect("valid unicode session");
    let encoded = bincode::serialize(&name).expect("session name encodes");
    let decoded: SessionName = bincode::deserialize(&encoded).expect("session name decodes");

    assert_eq!(decoded, name);
    assert_eq!(decoded.as_str(), "café-日本");
}

#[test]
fn target_session_name_accessor() {
    let session = Target::parse("alpha").expect("valid");
    assert_eq!(session.session_name().as_str(), "alpha");

    let window = Target::parse("alpha:0").expect("valid");
    assert_eq!(window.session_name().as_str(), "alpha");

    let pane = Target::parse("alpha:0.5").expect("valid");
    assert_eq!(pane.session_name().as_str(), "alpha");
}

#[test]
fn resize_pane_absolute_width_keeps_its_existing_bincode_tag() {
    let encoded = bincode::serialize(&ResizePaneAdjustment::AbsoluteWidth { columns: 34 })
        .expect("absolute width encodes");

    assert_eq!(encoded, [0, 0, 0, 0, 34, 0]);
    assert_eq!(
        bincode::deserialize::<ResizePaneAdjustment>(&encoded).expect("absolute width decodes"),
        ResizePaneAdjustment::AbsoluteWidth { columns: 34 }
    );
}

#[test]
fn resize_pane_adjustment_bincode_tags_keep_existing_order_append_only() {
    let cases = [
        (ResizePaneAdjustment::AbsoluteWidth { columns: 1 }, 0_u32),
        (ResizePaneAdjustment::AbsoluteHeight { rows: 1 }, 1),
        (ResizePaneAdjustment::Zoom, 2),
        (ResizePaneAdjustment::Up { cells: 1 }, 3),
        (ResizePaneAdjustment::Down { cells: 1 }, 4),
        (ResizePaneAdjustment::Left { cells: 1 }, 5),
        (ResizePaneAdjustment::Right { cells: 1 }, 6),
        (ResizePaneAdjustment::NoOp, 7),
        (
            ResizePaneAdjustment::AbsoluteSize {
                columns: 1,
                rows: 1,
            },
            8,
        ),
        (ResizePaneAdjustment::TrimBelow, 9),
        (
            ResizePaneAdjustment::Composite {
                columns: Some(1),
                rows: Some(1),
                relative: Some(crate::ResizePaneRelativeDirection::Right),
                cells: 1,
            },
            10,
        ),
    ];

    for (adjustment, expected_tag) in cases {
        let encoded = bincode::serialize(&adjustment).expect("adjustment encodes");
        assert_eq!(
            &encoded[..4],
            &expected_tag.to_le_bytes(),
            "{adjustment:?} must keep its bincode enum tag"
        );
    }
}

#[test]
fn layout_name_display_matches_tmux_names() {
    assert_eq!(LayoutName::MainVertical.to_string(), "main-vertical");
    assert_eq!(LayoutName::MainHorizontal.to_string(), "main-horizontal");
    assert_eq!(LayoutName::EvenHorizontal.to_string(), "even-horizontal");
    assert_eq!(LayoutName::EvenVertical.to_string(), "even-vertical");
    assert_eq!(LayoutName::Tiled.to_string(), "tiled");
    assert_eq!(
        LayoutName::MainHorizontalMirrored.to_string(),
        "main-horizontal-mirrored"
    );
    assert_eq!(
        LayoutName::MainVerticalMirrored.to_string(),
        "main-vertical-mirrored"
    );
}

#[test]
fn layout_name_bincode_tags_append_new_layouts_after_existing_variants() {
    for (layout, expected_tag) in [
        (LayoutName::MainVertical, 0_u32),
        (LayoutName::MainHorizontal, 1),
        (LayoutName::EvenHorizontal, 2),
        (LayoutName::EvenVertical, 3),
        (LayoutName::Tiled, 4),
        (LayoutName::MainHorizontalMirrored, 5),
        (LayoutName::MainVerticalMirrored, 6),
    ] {
        let encoded = bincode::serialize(&layout).expect("layout encodes");

        assert_eq!(encoded, expected_tag.to_le_bytes().to_vec());
        assert_eq!(
            bincode::deserialize::<LayoutName>(&encoded).expect("layout decodes"),
            layout
        );
    }
}

#[test]
fn option_name_bincode_tags_keep_v1_order_and_append_new_variants() {
    for (option, expected_tag) in [
        (OptionName::Status, 0_u32),
        (OptionName::DefaultTerminal, 1),
        (OptionName::TerminalFeatures, 2),
        (OptionName::PaneBorderStyle, 3),
        (OptionName::PaneActiveBorderStyle, 4),
        (OptionName::BufferLimit, 5),
        (OptionName::WindowStyle, 29),
        (OptionName::Backspace, 30),
        (OptionName::XtermKeys, 145),
        (OptionName::CopyModeLineNumbers, 146),
        (OptionName::CopyModeLineNumberStyle, 147),
        (OptionName::CopyModeCurrentLineNumberStyle, 148),
    ] {
        let encoded = bincode::serialize(&option).expect("option encodes");

        assert_eq!(encoded, expected_tag.to_le_bytes().to_vec());
        assert_eq!(
            bincode::deserialize::<OptionName>(&encoded).expect("option decodes"),
            option
        );
    }
}

#[test]
fn hook_name_bincode_tags_keep_client_attached_at_v1_tag_zero() {
    for (hook, expected_tag) in [
        (HookName::ClientAttached, 0_u32),
        (HookName::ClientDetached, 1),
        (HookName::ClientSessionChanged, 2),
        (HookName::SessionCreated, 3),
        (HookName::SessionClosed, 4),
        (HookName::SessionRenamed, 5),
        (HookName::SessionWindowChanged, 6),
        (HookName::WindowLinked, 7),
        (HookName::WindowUnlinked, 8),
        (HookName::WindowRenamed, 9),
        (HookName::WindowLayoutChanged, 10),
        (HookName::WindowPaneChanged, 11),
        (HookName::PaneExited, 12),
        (HookName::PaneModeChanged, 13),
        (HookName::PasteBufferChanged, 14),
        (HookName::PasteBufferDeleted, 15),
        (HookName::AfterSelectWindow, 16),
        (HookName::AfterSelectPane, 17),
        (HookName::AfterSendKeys, 18),
        (HookName::AfterSetOption, 19),
        (HookName::AfterBindKey, 20),
        (HookName::AfterNewSession, 34),
        (HookName::AfterSplitWindow, 52),
        (HookName::AlertActivity, 54),
        (HookName::ClientActive, 57),
        (HookName::CommandError, 63),
        (HookName::PaneDied, 64),
        (HookName::PaneTitleChanged, 68),
        (HookName::WindowResized, 69),
    ] {
        let encoded = bincode::serialize(&hook).expect("hook encodes");

        assert_eq!(encoded, expected_tag.to_le_bytes().to_vec());
        assert_eq!(
            bincode::deserialize::<HookName>(&encoded).expect("hook decodes"),
            hook
        );
    }

    assert!(bincode::deserialize::<HookName>(&70_u32.to_le_bytes()).is_err());
}

#[test]
fn scope_selector_bincode_tags_append_window_and_pane_variants() {
    let session = SessionName::new("alpha").expect("valid session");

    for (scope, expected_tag) in [
        (ScopeSelector::Global, 0_u32),
        (ScopeSelector::Session(session.clone()), 1),
        (
            ScopeSelector::Window(WindowTarget::with_window(session.clone(), 2)),
            2,
        ),
        (
            ScopeSelector::Pane(PaneTarget::with_window(session.clone(), 2, 3)),
            3,
        ),
    ] {
        let encoded = bincode::serialize(&scope).expect("scope encodes");

        assert_eq!(&encoded[..4], expected_tag.to_le_bytes().as_slice());
        assert_eq!(
            bincode::deserialize::<ScopeSelector>(&encoded).expect("scope decodes"),
            scope
        );
    }
}

#[test]
fn option_scope_selector_bincode_tags_are_stable_and_explicit() {
    let session = SessionName::new("alpha").expect("valid session");

    for (scope, expected_tag) in [
        (OptionScopeSelector::ServerGlobal, 0_u32),
        (OptionScopeSelector::SessionGlobal, 1),
        (OptionScopeSelector::WindowGlobal, 2),
        (OptionScopeSelector::Session(session.clone()), 3),
        (
            OptionScopeSelector::Window(WindowTarget::with_window(session.clone(), 2)),
            4,
        ),
        (
            OptionScopeSelector::Pane(PaneTarget::with_window(session.clone(), 2, 3)),
            5,
        ),
    ] {
        let encoded = bincode::serialize(&scope).expect("scope encodes");

        assert_eq!(&encoded[..4], expected_tag.to_le_bytes().as_slice());
        assert_eq!(
            bincode::deserialize::<OptionScopeSelector>(&encoded).expect("scope decodes"),
            scope
        );
    }
}

#[test]
fn layout_name_from_str_accepts_standard_layout_names() {
    for (layout_name, expected) in [
        ("main-vertical", LayoutName::MainVertical),
        ("main-horizontal", LayoutName::MainHorizontal),
        ("even-horizontal", LayoutName::EvenHorizontal),
        ("even-vertical", LayoutName::EvenVertical),
        ("tiled", LayoutName::Tiled),
        (
            "main-horizontal-mirrored",
            LayoutName::MainHorizontalMirrored,
        ),
        ("main-vertical-mirrored", LayoutName::MainVerticalMirrored),
    ] {
        assert_eq!(layout_name.parse::<LayoutName>().unwrap(), expected);
    }
}

#[test]
fn select_layout_standard_layouts_round_trip_through_frame_codec() {
    let session = SessionName::new("alpha").expect("valid session");

    for layout in [
        LayoutName::MainHorizontal,
        LayoutName::MainHorizontalMirrored,
        LayoutName::MainVerticalMirrored,
        LayoutName::EvenHorizontal,
        LayoutName::EvenVertical,
        LayoutName::Tiled,
    ] {
        let request = Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(session.clone())),
            layout,
        });

        let frame = encode_frame(&request).expect("request encodes");
        let decoded: Request = decode_frame(&frame).expect("request decodes");

        assert_eq!(decoded, request);
    }
}
