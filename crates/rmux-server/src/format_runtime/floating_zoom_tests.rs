//! Zoom's displayed floating formats, measured on tmux 3.7b.
use super::RuntimeFormatContext;
use crate::session_fixture::SessionFixture;
use rmux_core::formats::{render_template, FormatContext};
use rmux_core::FloatingPaneRequest;
use rmux_proto::{SessionName, TerminalSize, WindowTarget};

const FORMAT: &str = "#{pane_floating_flag}|#{pane_x},#{pane_y},#{pane_z}|#{pane_width}x#{pane_height}|#{pane_left},#{pane_top}";

#[test]
fn floating_option_geometry_requires_the_targeted_pane() {
    use rmux_core::formats::FormatVariables;

    let mut fixture = SessionFixture::new(
        SessionName::new("floating-no-pane-format").unwrap(),
        TerminalSize::new(80, 24),
    );
    let target = WindowTarget::with_window(fixture.name().clone(), 0);
    let _ = fixture
        .store
        .new_floating_pane(
            &target,
            FloatingPaneRequest::new(Some(3), Some(4), 30, 8).unwrap(),
            false,
        )
        .unwrap();
    let session = fixture.view();
    let context = RuntimeFormatContext::new(FormatContext::new())
        .with_session(session)
        .with_window(0, session.window());
    for name in [
        "pane_x",
        "pane_y",
        "pane_at_left",
        "pane_at_right",
        "pane_at_top",
        "pane_at_bottom",
    ] {
        assert_eq!(
            context.format_value_by_name(name).as_deref(),
            Some(""),
            "{name}"
        );
        assert_eq!(context.format_path_value_by_name(name), None, "{name}");
    }
}

#[test]
fn floating_scrollbar_option_formats_use_the_unclipped_content_rectangle() {
    use rmux_core::OptionStore;
    use rmux_proto::{OptionName, ScopeSelector, SetOptionMode};

    let mut fixture = SessionFixture::new(
        SessionName::new("floating-scrollbar-formats").unwrap(),
        TerminalSize::new(80, 24),
    );
    let target = WindowTarget::with_window(fixture.name().clone(), 0);
    let created = fixture
        .store
        .new_floating_pane(
            &target,
            FloatingPaneRequest::new(Some(3), Some(4), 30, 8).unwrap(),
            true,
        )
        .unwrap();
    let mut options = OptionStore::new();
    for (option, value) in [
        (OptionName::PaneScrollbars, "on"),
        (OptionName::PaneScrollbarsPosition, "left"),
        (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
    ] {
        options
            .set(
                ScopeSelector::Window(target.clone()),
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    let session = fixture.view();
    let window = session.window();
    let context = RuntimeFormatContext::new(FormatContext::new())
        .with_session(session)
        .with_window(0, window)
        .with_pane(window.pane(created.pane_index).unwrap())
        .with_options(&options);
    // Measured on Linux tmux 3.7b: the left track and pad precede
    // the floating pane's content, just as they do for the tiled pane.
    assert_eq!(
        render_template(
            "#{pane_left}|#{pane_top}|#{pane_width}|#{pane_height}|#{pane_right}|#{pane_bottom}|#{pane_x}|#{pane_y}",
            &context,
        ),
        "6|4|27|8|32|11|6|4"
    );
}

#[test]
fn floating_scrollbar_option_formats_keep_signed_offsets_and_exact_edges() {
    use rmux_core::OptionStore;
    use rmux_proto::{OptionName, ScopeSelector, SetOptionMode};

    let mut fixture = SessionFixture::new(
        SessionName::new("floating-scrollbar-edges").unwrap(),
        TerminalSize::new(80, 24),
    );
    let target = WindowTarget::with_window(fixture.name().clone(), 0);
    for (x, y) in [(-3, -2), (0, 0), (70, 20), (80, 24)] {
        let _ = fixture
            .store
            .new_floating_pane(
                &target,
                FloatingPaneRequest::new(Some(x), Some(y), 30, 8).unwrap(),
                true,
            )
            .unwrap();
    }
    let template = "#{pane_left}|#{pane_top}|#{pane_width}|#{pane_height}|#{pane_right}|#{pane_bottom}|#{pane_at_left}|#{pane_at_right}|#{pane_at_top}|#{pane_at_bottom}|#{pane_x}|#{pane_y}";
    let mut options = OptionStore::new();
    for (option, value) in [
        (OptionName::PaneScrollbars, "on"),
        (OptionName::PaneScrollbarsPosition, "left"),
        (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
    ] {
        options
            .set(
                ScopeSelector::Window(target.clone()),
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    // Frozen Linux tmux 3.7b matrix: the off-screen content is not clipped,
    // and extending past an edge does not mean ending at that edge.
    for (border, expected) in [
        (
            "off",
            [
                "0|-2|27|8|26|5|1|0|0|0|0|-2",
                "3|0|27|8|29|7|0|0|1|0|3|0",
                "73|20|27|8|99|27|0|0|0|0|73|20",
                "83|24|27|8|109|31|0|0|0|0|83|24",
            ],
        ),
        (
            "top",
            [
                "0|-2|27|8|26|5|1|0|0|0|0|-2",
                "3|0|27|8|29|7|0|0|0|0|3|0",
                "73|20|27|8|99|27|0|0|0|0|73|20",
                "83|24|27|8|109|31|0|0|0|0|83|24",
            ],
        ),
        (
            "bottom",
            [
                "0|-2|27|8|26|5|1|0|0|0|0|-2",
                "3|0|27|8|29|7|0|0|1|0|3|0",
                "73|20|27|8|99|27|0|0|0|0|73|20",
                "83|24|27|8|109|31|0|0|0|0|83|24",
            ],
        ),
    ] {
        options
            .set(
                ScopeSelector::Window(target.clone()),
                OptionName::PaneBorderStatus,
                border.to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
        let session = fixture.view();
        for (slot, expected) in (1..=4).zip(expected) {
            let context = RuntimeFormatContext::new(FormatContext::new())
                .with_session(session)
                .with_window(0, session.window())
                .with_pane(session.window().pane(slot).unwrap())
                .with_options(&options);
            assert_eq!(
                render_template(template, &context),
                expected,
                "{border}, pane {slot}"
            );
        }
    }
}

fn rendered(fixture: &SessionFixture, index: u32) -> String {
    let session = fixture.view();
    let window = session.window();
    let context = RuntimeFormatContext::new(FormatContext::new())
        .with_session(session)
        .with_window(0, window)
        .with_pane(window.pane(index).unwrap());
    render_template(FORMAT, &context)
}

#[test]
fn floating_zoom_formats_use_the_displayed_layer_and_restore_canonical_placements() {
    let mut fixture = SessionFixture::new(
        SessionName::new("floating-zoom-formats").unwrap(),
        TerminalSize::new(80, 24),
    );
    let target = WindowTarget::with_window(fixture.name().clone(), 0);
    for (x, y, cols, rows) in [(-3, -2, 30, 7), (8, 4, 20, 5)] {
        let _ = fixture
            .store
            .new_floating_pane(
                &target,
                FloatingPaneRequest::new(Some(x), Some(y), cols, rows).unwrap(),
                true,
            )
            .unwrap();
    }
    for active in 0..3 {
        fixture.select_pane(active).unwrap();
        let canonical = fixture.window().floating_stack();
        let before: Vec<_> = (0..3).map(|pane| rendered(&fixture, pane)).collect();
        fixture.toggle_zoom_in_window(0, active).unwrap();
        for (pane, unzoomed) in [
            "0|0,0,1|80x24|0,0",
            "0|-3,-2,1|30x7|-3,-2",
            "0|8,4,1|20x5|8,4",
        ]
        .into_iter()
        .enumerate()
        {
            let expected = if pane as u32 == active {
                "0|0,0,1|80x24|0,0"
            } else {
                unzoomed
            };
            assert_eq!(
                rendered(&fixture, pane as u32),
                expected,
                "zoom {active}, pane {pane}"
            );
        }
        assert_eq!(fixture.window().floating_stack(), canonical);
        fixture.toggle_zoom_in_window(0, active).unwrap();
        assert_eq!(
            (0..3)
                .map(|pane| rendered(&fixture, pane))
                .collect::<Vec<_>>(),
            before
        );
    }
}

#[test]
fn floating_resize_formats_preserve_overhanging_tiled_content() {
    let mut fixture = SessionFixture::new(
        SessionName::new("floating-resize-formats").unwrap(),
        TerminalSize::new(80, 24),
    );
    let target = WindowTarget::with_window(fixture.name().clone(), 0);
    let created = fixture
        .store
        .new_floating_pane(
            &target,
            FloatingPaneRequest::new(Some(-3), Some(-2), 30, 7).unwrap(),
            true,
        )
        .unwrap();
    fixture
        .resize_pane(
            created.pane_index,
            rmux_proto::ResizePaneAdjustment::AbsoluteHeight { rows: 5 },
        )
        .unwrap();
    // tmux transfers two rows to the tile, even beyond the visible window.
    assert_eq!(rendered(&fixture, 0), "0|0,0,2|80x26|0,0");
    assert_eq!(
        rendered(&fixture, created.pane_index),
        "1|-3,-2,0|30x5|-3,-2"
    );
    assert_eq!(fixture.window().size(), TerminalSize::new(80, 24));
}
