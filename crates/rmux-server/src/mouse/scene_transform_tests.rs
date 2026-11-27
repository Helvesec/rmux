use super::*;
use crate::pane_terminals::HandlerState;
use rmux_core::{FloatingPaneRect, ManagedPaneSplit};
use rmux_proto::{
    OptionName, ScopeSelector, SetOptionMode, SplitDirection, TerminalSize, WindowTarget,
};

fn fixture_state(x: i32, scrollbar: bool) -> (HandlerState, SessionName) {
    let name = SessionName::new("scene-transform").unwrap();
    let target = WindowTarget::with_window(name.clone(), 0);
    let mut state = HandlerState::default();
    state
        .sessions
        .create_session(name.clone(), TerminalSize { cols: 20, rows: 10 })
        .unwrap();
    state
        .options
        .set(
            ScopeSelector::Session(name.clone()),
            OptionName::Status,
            "off".into(),
            SetOptionMode::Replace,
        )
        .unwrap();
    for (option, value) in [
        (OptionName::PaneBorderStatus, "off"),
        (
            OptionName::PaneScrollbars,
            if scrollbar { "on" } else { "off" },
        ),
        (OptionName::PaneScrollbarsPosition, "left"),
    ] {
        state
            .options
            .set(
                ScopeSelector::Window(target.clone()),
                option,
                value.into(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    let pane = state
        .sessions
        .split_pane(
            &target,
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Vertical,
                before: false,
                detached: false,
                full_size: false,
            },
        )
        .unwrap();
    state
        .sessions
        .window_layout_context_mut(&target)
        .unwrap()
        .float_pane(pane.pane_index, FloatingPaneRect::new(x, 2, 8, 4).unwrap())
        .unwrap();
    (state, name)
}

fn fixture(x: i32, scrollbar: bool) -> MouseLayout {
    let (state, name) = fixture_state(x, scrollbar);
    layout_for_session(&state, &name, 1).unwrap()
}

fn event(layout: &MouseLayout, x: u16, y: u16) -> AttachedMouseEvent {
    let raw = MouseForwardEvent {
        x,
        y,
        lx: x,
        ly: y,
        b: 0,
        lb: 0,
        sgr_b: 0,
        sgr_type: 'M',
        ignore: false,
    };
    hit_to_attached_event(
        layout,
        raw,
        resolve_mouse_hit(layout, x, y, false, None),
        false,
    )
    .unwrap()
}

#[test]
fn signed_source_copy_coordinates() {
    let layout = fixture(-3, false);
    let pane = layout.panes.iter().max_by_key(|pane| pane.layer).unwrap();
    let event = event(&layout, 0, 2);
    assert_eq!(event.pane_id, Some(pane.pane_id));
    let context = copy_mode_mouse_context(&event, pane.coordinates(), -1).unwrap();
    assert_eq!((context.content_x, context.content_y), (3, 0));
}

#[test]
fn clipped_left_track_is_not_recreated_at_viewport_edge() {
    let layout = fixture(-3, true);
    assert_eq!(event(&layout, 0, 2).location, MouseLocation::Pane);
}

#[test]
fn foreground_frame_wins_over_background_content() {
    let layout = fixture(5, false);
    let pane = layout.panes.iter().max_by_key(|pane| pane.layer).unwrap();
    let event = event(&layout, 4, 2);
    assert_eq!(event.pane_id, Some(pane.pane_id));
    assert_eq!(event.location, MouseLocation::Border);
}

#[test]
fn floating_translation_updates_scene_hits_without_reordering_layers() {
    let (mut state, name) = fixture_state(1, false);
    let target = WindowTarget::with_window(name.clone(), 0);
    let rear_id = state
        .sessions
        .session(&name)
        .unwrap()
        .window_at(0)
        .unwrap()
        .pane(1)
        .unwrap()
        .id();
    let front = state
        .sessions
        .split_pane(
            &target,
            0,
            ManagedPaneSplit {
                direction: SplitDirection::Vertical,
                before: false,
                detached: true,
                full_size: false,
            },
        )
        .unwrap();
    state
        .sessions
        .window_layout_context_mut(&target)
        .unwrap()
        .float_pane(front.pane_index, FloatingPaneRect::new(6, 2, 8, 4).unwrap())
        .unwrap();
    let before = layout_for_session(&state, &name, 1).unwrap();
    let rear_index = state
        .sessions
        .session(&name)
        .unwrap()
        .window_at(0)
        .unwrap()
        .panes()
        .iter()
        .find(|pane| pane.id() == rear_id)
        .unwrap()
        .index();
    assert_eq!(event(&before, 2, 3).pane_id, Some(rear_id));
    assert_eq!(event(&before, 7, 3).pane_id, Some(front.pane_id));

    state
        .sessions
        .window_layout_context_mut(&target)
        .unwrap()
        .translate_floating_pane(rear_index, Some(10), None)
        .unwrap();
    let after = layout_for_session(&state, &name, 1).unwrap();
    assert_ne!(event(&after, 2, 3).pane_id, Some(rear_id));
    assert_eq!(event(&after, 17, 3).pane_id, Some(rear_id));
    assert_eq!(event(&after, 11, 3).pane_id, Some(front.pane_id));

    for position in ["top", "bottom"] {
        for (option, value) in [
            (OptionName::Status, "on"),
            (OptionName::StatusPosition, position),
        ] {
            state
                .options
                .set(
                    ScopeSelector::Session(name.clone()),
                    option,
                    value.into(),
                    SetOptionMode::Replace,
                )
                .unwrap();
        }
        let layout = layout_for_session(&state, &name, 1).unwrap();
        let status_offset = u16::from(layout.status_at == Some(0));
        assert_eq!(
            event(&layout, 17, 3 + status_offset).pane_id,
            Some(rear_id),
            "status {position} must not change logical floating coordinates"
        );
        assert_eq!(
            event(&layout, 11, 3 + status_offset).pane_id,
            Some(front.pane_id),
            "status {position} must preserve frontmost overlap hits"
        );
    }
}

#[test]
fn signed_scrollbar_padding_and_local_conversion_matrix() {
    use crate::pane_scrollbar::PaneScrollbarConfig;
    use rmux_core::{ResolvedPaneBorderStatus, ResolvedSceneContext, ResolvedZoomVisibility};
    for position in [ScrollbarPosition::Left, ScrollbarPosition::Right] {
        for (x, y) in [(-3, 2), (17, 2), (5, -2)] {
            for pad in [0, 1, 20] {
                let (mut state, name) = fixture_state(x, false);
                state
                    .sessions
                    .window_layout_context_mut(&WindowTarget::with_window(name.clone(), 0))
                    .unwrap()
                    .move_floating_pane(1, x, y)
                    .unwrap();
                let window = state.sessions.session(&name).unwrap().window_at(0).unwrap();
                let scene = window.resolved_scene(ResolvedSceneContext::new(
                    TerminalSize { cols: 20, rows: 10 },
                    ResolvedPaneBorderStatus::Off,
                    ResolvedZoomVisibility::Rendered,
                ));
                let pane_scene = scene.pane_by_index(1).unwrap();
                let config = PaneScrollbarConfig {
                    mode: PaneScrollbarsMode::On,
                    position,
                    style: Default::default(),
                    width: 1,
                    pad,
                };
                let (projection, local) =
                    projection::MouseProjection::new(pane_scene, &config, false, false);
                let reserved = i32::from(7.min(pad + 1));
                let source_x = x + if position == ScrollbarPosition::Left {
                    reserved
                } else {
                    0
                };
                assert_eq!(projection.coordinates.source.x(), source_x);
                assert_eq!(projection.coordinates.source.cols(), 8 - reserved);
                assert_eq!(projection.coordinates.source.y(), y);
                assert_eq!(local.content.rows(), 4);
                for screen_y in 0..10 {
                    for screen_x in 0..20 {
                        let dx = i32::from(screen_x) - source_x;
                        let dy = i32::from(screen_y) - y;
                        let expected = ((0..8 - reserved).contains(&dx) && (0..4).contains(&dy))
                            .then_some((dx as u16, dy as u16));
                        assert_eq!(
                            projection.coordinates.relative_position(screen_x, screen_y),
                            expected,
                            "{position:?} {x},{y} pad={pad} screen={screen_x},{screen_y}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn signed_copy_drag_line_numbers_and_top_status_preserve_source() {
    let layout = fixture(-3, false);
    let pane = layout.panes.iter().max_by_key(|pane| pane.layer).unwrap();
    let mut event = event(&layout, 2, 2);
    event.status_at = Some(0);
    event.status_lines = 2;
    event.raw.y = 4;
    event.raw.b = 32;
    event.raw.lx = 1;
    event.raw.ly = 4;
    let numbers = CopyModeLineNumberLayout::resolve(Some("absolute"), true, 10, 4, 0, 0);
    let context = copy_mode_mouse_drag_start_context_with_line_numbers(
        &event,
        pane.coordinates(),
        -1,
        numbers,
    )
    .unwrap();
    assert_eq!((context.content_x, context.content_y), (1, 0));
    assert_eq!(context.selection_anchor, Some((0, 0)));
    assert_eq!(context.scroll_y, 2);
    event.raw.x = 30;
    event.raw.y = 30;
    let context = copy_mode_mouse_context(&event, pane.coordinates(), -1).unwrap();
    assert_eq!((context.content_x, context.content_y), (7, 3));
}

#[test]
fn target_conversion_and_scene_hits_share_status_viewport() {
    for status in ["off", "on", "2"] {
        for position in ["top", "bottom"] {
            for border in ["off", "top", "bottom"] {
                let (mut state, name) = fixture_state(-3, true);
                for (option, value) in [
                    (OptionName::Status, status),
                    (OptionName::StatusPosition, position),
                ] {
                    state
                        .options
                        .set(
                            ScopeSelector::Session(name.clone()),
                            option,
                            value.into(),
                            SetOptionMode::Replace,
                        )
                        .unwrap();
                }
                state
                    .options
                    .set(
                        ScopeSelector::Window(WindowTarget::with_window(name.clone(), 0)),
                        OptionName::PaneBorderStatus,
                        border.into(),
                        SetOptionMode::Replace,
                    )
                    .unwrap();
                let layout = layout_for_session(&state, &name, 1).unwrap();
                for pane in &layout.panes {
                    let coordinates = pane_mouse_coordinates_for_target(
                        &state,
                        pane.pane_target.as_ref().unwrap(),
                    )
                    .unwrap();
                    assert_eq!(
                        coordinates,
                        pane.coordinates(),
                        "{status} {position} {border}"
                    );
                }
                let pane = layout.panes.iter().max_by_key(|pane| pane.layer).unwrap();
                let offset = if layout.status_at == Some(0) {
                    layout.status_lines
                } else {
                    0
                };
                let event = event(&layout, 0, 2 + offset);
                assert_eq!(event.pane_id, Some(pane.pane_id));
                assert_eq!(event.location, MouseLocation::Pane);
                assert_eq!(pane.coordinates().relative_position(0, 2), Some((2, 0)));
                let context = copy_mode_mouse_context(&event, pane.coordinates(), -1).unwrap();
                assert_eq!((context.content_x, context.content_y), (2, 0));
            }
        }
    }
}

#[test]
fn tiled_scene_status_scrollbar_and_padding_hits() {
    for status in ["off", "on", "2"] {
        for status_position in ["top", "bottom"] {
            for border in ["off", "top", "bottom"] {
                for position in ["left", "right"] {
                    let name = SessionName::new("tiled-mouse-scene").unwrap();
                    let mut state = HandlerState::default();
                    state
                        .sessions
                        .create_session(name.clone(), TerminalSize { cols: 20, rows: 10 })
                        .unwrap();
                    for (option, value) in [
                        (OptionName::Status, status),
                        (OptionName::StatusPosition, status_position),
                    ] {
                        state
                            .options
                            .set(
                                ScopeSelector::Session(name.clone()),
                                option,
                                value.into(),
                                SetOptionMode::Replace,
                            )
                            .unwrap();
                    }
                    for (option, value) in [
                        (OptionName::PaneBorderStatus, border),
                        (OptionName::PaneScrollbars, "on"),
                        (OptionName::PaneScrollbarsPosition, position),
                        (OptionName::PaneScrollbarsStyle, "width=1,pad=1"),
                    ] {
                        state
                            .options
                            .set(
                                ScopeSelector::Window(WindowTarget::with_window(name.clone(), 0)),
                                option,
                                value.into(),
                                SetOptionMode::Replace,
                            )
                            .unwrap();
                    }
                    let layout = layout_for_session(&state, &name, 1).unwrap();
                    let pane = &layout.panes[0];
                    let offset = if layout.status_at == Some(0) {
                        layout.status_lines
                    } else {
                        0
                    };
                    let top = u16::from(border == "top");
                    let rows = 10 - layout.status_lines - u16::from(border != "off");
                    let content_x = if position == "left" { 2 } else { 0 };
                    assert_eq!(
                        pane.geometry,
                        rmux_core::PaneGeometry::new(content_x, top, 18, rows)
                    );
                    let track = if position == "left" { 0 } else { 19 };
                    let pad = if position == "left" { 1 } else { 18 };
                    assert_eq!(
                        event(&layout, track, top + offset).location,
                        MouseLocation::ScrollbarSlider
                    );
                    assert_eq!(
                        event(&layout, pad, top + offset).location,
                        MouseLocation::Pane
                    );
                    assert_eq!(pane.coordinates().relative_position(pad, top), None);
                    assert_eq!(
                        pane.coordinates().relative_position(content_x, top),
                        Some((0, 0))
                    );
                    if border != "off" {
                        let border_y = if border == "top" { 0 } else { rows };
                        assert_eq!(
                            event(&layout, track, border_y + offset).location,
                            MouseLocation::Border
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn vertically_clipped_track_keeps_slider_source_offset() {
    for position in ["left", "right"] {
        let (mut state, name) = fixture_state(5, true);
        let window = WindowTarget::with_window(name.clone(), 0);
        state
            .sessions
            .window_layout_context_mut(&window)
            .unwrap()
            .move_floating_pane(1, 5, -2)
            .unwrap();
        state
            .options
            .set(
                ScopeSelector::Window(window),
                OptionName::PaneScrollbarsPosition,
                position.into(),
                SetOptionMode::Replace,
            )
            .unwrap();
        let layout = layout_for_session(&state, &name, 1).unwrap();
        let pane = layout.panes.iter().max_by_key(|pane| pane.layer).unwrap();
        assert_eq!(pane.scrollbar.unwrap().slider_h, 4);
        let x = if position == "left" { 5 } else { 12 };
        let hit = resolve_mouse_hit(&layout, x, 0, false, None);
        assert_eq!(hit.location, MouseLocation::ScrollbarSlider);
        assert_eq!(hit.slider_mpos, Some(2));
    }
}
