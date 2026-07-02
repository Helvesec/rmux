use super::{
    classify_mouse_event, classify_mouse_events, copy_mode_mouse_context, MouseDragHandler,
    MouseForwardEvent, MouseLayout, MouseLocation, PaneBorderStatus, PaneMouseTarget,
    PaneScrollbar, PaneScrollbarsMode, ScrollbarPosition, StatusLineLayout, StatusRange,
    StatusRangeType,
};
use rmux_core::{key_string_lookup_string, PaneGeometry, PaneId};
use rmux_proto::{PaneTarget, SessionName};
use std::time::Instant;

fn pane_target(index: u32) -> PaneTarget {
    PaneTarget::with_window(SessionName::new("alpha").expect("valid session"), 0, index)
}

fn layout() -> MouseLayout {
    MouseLayout {
        session_id: 1,
        status_at: Some(0),
        status_lines: 1,
        status: Some(StatusLineLayout {
            ranges: vec![
                StatusRange {
                    x: 0..=3,
                    kind: StatusRangeType::Left,
                },
                StatusRange {
                    x: 4..=7,
                    kind: StatusRangeType::Right,
                },
                StatusRange {
                    x: 8..=11,
                    kind: StatusRangeType::Window(9),
                },
                StatusRange {
                    x: 12..=15,
                    kind: StatusRangeType::Session(4),
                },
                StatusRange {
                    x: 16..=19,
                    kind: StatusRangeType::User,
                },
                StatusRange {
                    x: 20..=23,
                    kind: StatusRangeType::Control(3),
                },
            ],
        }),
        pane_border_status: PaneBorderStatus::Off,
        focus_follows_mouse: true,
        active_pane: Some(PaneId::new(0)),
        panes: vec![
            PaneMouseTarget {
                pane_id: PaneId::new(0),
                pane_target: Some(pane_target(0)),
                window_id: 5,
                geometry: PaneGeometry::new(0, 0, 40, 10),
                scrollbar: Some(
                    PaneScrollbar::from_view(
                        10,
                        30,
                        false,
                        PaneScrollbarsMode::On,
                        ScrollbarPosition::Right,
                        1,
                        0,
                        None,
                    )
                    .expect("scrollbar"),
                ),
                border_controls: vec![super::BorderControlRange {
                    x: 41..=41,
                    y: 4,
                    control: 2,
                }],
            },
            PaneMouseTarget {
                pane_id: PaneId::new(1),
                pane_target: Some(pane_target(1)),
                window_id: 5,
                geometry: PaneGeometry::new(41, 0, 39, 10),
                scrollbar: None,
                border_controls: Vec::new(),
            },
        ],
    }
}

fn raw(b: u16, x: u16, y: u16) -> MouseForwardEvent {
    MouseForwardEvent {
        b,
        lb: b,
        x,
        y,
        lx: x,
        ly: y,
        sgr_b: b,
        sgr_type: ' ',
        ignore: false,
    }
}

#[test]
fn status_ranges_hit_left_right_window_session_user_and_control() {
    let mut state = super::ClientMouseState::default();
    let now = Instant::now();

    let left = classify_mouse_event(&mut state, &layout(), raw(0, 1, 0), now).expect("left");
    assert_eq!(left.event.location, MouseLocation::StatusLeft);

    let right = classify_mouse_event(&mut state, &layout(), raw(0, 5, 0), now).expect("right");
    assert_eq!(right.event.location, MouseLocation::StatusRight);

    let window = classify_mouse_event(&mut state, &layout(), raw(0, 9, 0), now).expect("window");
    assert_eq!(window.event.location, MouseLocation::Status);
    assert_eq!(window.event.window_id, Some(9));

    let session = classify_mouse_event(&mut state, &layout(), raw(0, 13, 0), now).expect("session");
    assert_eq!(session.event.session_id, 4);

    let user = classify_mouse_event(&mut state, &layout(), raw(0, 17, 0), now).expect("user");
    assert_eq!(user.event.location, MouseLocation::Status);

    let control = classify_mouse_event(&mut state, &layout(), raw(0, 22, 0), now).expect("control");
    assert_eq!(control.event.location, MouseLocation::Control(3));
}

#[test]
fn click_sequence_resets_on_button_location_or_pane_change() {
    let mut state = super::ClientMouseState::default();
    let base = Instant::now();
    let layout = layout();

    let down = classify_mouse_event(&mut state, &layout, raw(0, 5, 5), base).expect("down");
    assert_eq!(down.event.location, MouseLocation::Pane);
    assert_eq!(
        down.key,
        key_string_lookup_string("MouseDown1Pane").unwrap()
    );

    let second = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(0, 5, 5)
        },
        base + std::time::Duration::from_millis(100),
    )
    .expect("second");
    assert_eq!(
        second.key,
        key_string_lookup_string("SecondClick1Pane").unwrap()
    );

    let reset = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(0, 50, 5)
        },
        base + std::time::Duration::from_millis(150),
    )
    .expect("reset");
    assert_eq!(
        reset.key,
        key_string_lookup_string("MouseDown1Pane").unwrap()
    );

    let expired = state.expire_click_timer(base + std::time::Duration::from_millis(700), &layout);
    assert!(expired.is_none(), "first click timeout just resets state");
}

#[test]
fn second_click_timeout_yields_double_click_and_third_click_has_no_timer() {
    let mut state = super::ClientMouseState::default();
    let base = Instant::now();
    let layout = layout();

    let _ = classify_mouse_event(&mut state, &layout, raw(0, 5, 5), base);
    let _ = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(0, 5, 5)
        },
        base + std::time::Duration::from_millis(100),
    );
    let double = state
        .expire_click_timer(base + std::time::Duration::from_millis(500), &layout)
        .expect("double click");
    assert_eq!(
        double.key,
        key_string_lookup_string("DoubleClick1Pane").unwrap()
    );

    let _ = classify_mouse_event(&mut state, &layout, raw(0, 5, 5), base);
    let _ = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(0, 5, 5)
        },
        base + std::time::Duration::from_millis(100),
    );
    let triple = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(0, 5, 5)
        },
        base + std::time::Duration::from_millis(150),
    )
    .expect("triple");
    assert_eq!(
        triple.key,
        key_string_lookup_string("TripleClick1Pane").unwrap()
    );
    assert!(
        state.click_deadline.is_none(),
        "triple click skips the timer"
    );
}

#[test]
fn late_third_click_does_not_drop_pending_double_click() {
    let mut state = super::ClientMouseState::default();
    let base = Instant::now();
    let layout = layout();

    let _ = classify_mouse_event(&mut state, &layout, raw(0, 5, 5), base);
    let _ = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(0, 5, 5)
        },
        base + std::time::Duration::from_millis(100),
    );

    let events = classify_mouse_events(
        &mut state,
        &layout,
        MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(0, 5, 5)
        },
        base + std::time::Duration::from_millis(500),
    );

    assert_eq!(
        events.iter().map(|event| event.key).collect::<Vec<_>>(),
        vec![
            key_string_lookup_string("DoubleClick1Pane").unwrap(),
            key_string_lookup_string("MouseDown1Pane").unwrap(),
        ],
        "the first two clicks should still dispatch DoubleClick1Pane before the late click"
    );
}

#[test]
fn drag_update_uses_dragging_sentinel_and_release_synthesizes_drag_end() {
    let mut state = super::ClientMouseState {
        drag_handler: Some(MouseDragHandler::CopyModeSelection {
            target: pane_target(0),
        }),
        ..Default::default()
    };
    let now = Instant::now();
    let layout = layout();

    let start = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            b: 32,
            lb: 0,
            x: 6,
            y: 6,
            lx: 5,
            ly: 5,
            sgr_b: 32,
            sgr_type: ' ',
            ignore: false,
        },
        now,
    )
    .expect("drag start");
    assert_eq!(start.key, super::KEYC_DRAGGING);
    assert_eq!(start.event.raw.x, 5);
    assert_eq!(start.event.raw.y, 5);
    let context = copy_mode_mouse_context(&start.event, layout.panes[0].geometry, -1)
        .expect("copy-mode drag start context");
    assert_eq!(context.content_x, 5);
    assert_eq!(context.content_y, 4);
    assert_eq!(state.drag_flag, 1);

    let end = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            b: 3,
            lb: 0,
            x: 7,
            y: 7,
            lx: 6,
            ly: 6,
            sgr_b: 0,
            sgr_type: ' ',
            ignore: false,
        },
        now + std::time::Duration::from_millis(10),
    )
    .expect("drag end");
    assert_eq!(
        end.key,
        key_string_lookup_string("MouseDragEnd1Pane").unwrap()
    );
    assert_eq!(state.drag_flag, 0);
    assert_eq!(state.slider_mpos, -1);
}

#[test]
fn plain_mouse_drag_keeps_current_coordinates_for_bindings() {
    let mut state = super::ClientMouseState::default();
    let now = Instant::now();
    let layout = layout();

    let drag = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            b: 32,
            lb: 0,
            x: 6,
            y: 6,
            lx: 5,
            ly: 5,
            sgr_b: 32,
            sgr_type: 'M',
            ignore: false,
        },
        now,
    )
    .expect("drag event");

    assert_eq!(
        drag.key,
        key_string_lookup_string("MouseDrag1Pane").unwrap()
    );
    assert_eq!(drag.event.raw.x, 6);
    assert_eq!(drag.event.raw.y, 6);
}

#[test]
fn scrollbar_drag_forces_slider_location_and_tracks_relative_mpos() {
    let mut state = super::ClientMouseState::default();
    let mut layout = layout();
    layout.panes[0].scrollbar = Some(PaneScrollbar {
        position: ScrollbarPosition::Right,
        width: 1,
        pad: 0,
        slider_y: 2,
        slider_h: 3,
    });
    let now = Instant::now();

    let drag = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            b: 32,
            lb: 0,
            x: 40,
            y: 4,
            lx: 40,
            ly: 4,
            sgr_b: 32,
            sgr_type: ' ',
            ignore: false,
        },
        now,
    )
    .expect("scrollbar drag");
    assert_eq!(drag.event.location, MouseLocation::ScrollbarSlider);
    assert!(state.scrolling_flag);
    assert_eq!(state.slider_mpos, 1 + i32::from(layout.status_lines));

    let move_outside = classify_mouse_event(
        &mut state,
        &layout,
        MouseForwardEvent {
            b: 32,
            lb: 0,
            x: 10,
            y: 8,
            lx: 40,
            ly: 4,
            sgr_b: 32,
            sgr_type: ' ',
            ignore: false,
        },
        now + std::time::Duration::from_millis(5),
    )
    .expect("forced slider location");
    assert_eq!(move_outside.event.location, MouseLocation::ScrollbarSlider);
}

#[test]
fn focus_follows_mouse_returns_the_new_pane_target() {
    let mut state = super::ClientMouseState::default();
    let event = classify_mouse_event(
        &mut state,
        &layout(),
        MouseForwardEvent {
            b: 35,
            lb: 35,
            x: 50,
            y: 6,
            lx: 49,
            ly: 6,
            sgr_b: 35,
            sgr_type: 'm',
            ignore: false,
        },
        Instant::now(),
    )
    .expect("move");
    assert_eq!(
        event.key,
        key_string_lookup_string("MouseMovePane").unwrap()
    );
    assert_eq!(event.focus_target, Some(PaneId::new(1)));
}

#[test]
fn border_controls_win_over_plain_border_hits() {
    let mut state = super::ClientMouseState::default();
    let event =
        classify_mouse_event(&mut state, &layout(), raw(0, 41, 5), Instant::now()).expect("border");
    assert_eq!(event.event.location, MouseLocation::Control(2));
}

#[test]
fn copy_mode_mouse_context_converts_to_content_coordinates() {
    let event = super::AttachedMouseEvent {
        raw: raw(0, 10, 6),
        session_id: 1,
        window_id: Some(5),
        pane_id: Some(PaneId::new(0)),
        pane_target: Some(pane_target(0)),
        location: MouseLocation::Pane,
        status_at: Some(0),
        status_lines: 1,
        ignore: false,
    };
    let context =
        copy_mode_mouse_context(&event, PaneGeometry::new(0, 0, 40, 10), 3).expect("context");
    assert_eq!(context.content_x, 10);
    assert_eq!(context.content_y, 5);
    assert_eq!(context.scroll_y, 6);
    assert_eq!(context.slider_mpos, 3);
}

#[test]
fn expire_click_timer_is_noop_when_no_deadline_set() {
    let mut state = super::ClientMouseState::default();
    assert!(state.click_deadline.is_none());
    state.triple_click_pending = true; // should not trigger DoubleClick
    let result = state.expire_click_timer(Instant::now(), &layout());
    assert!(result.is_none());
    // triple_click_pending is NOT cleared when there is no deadline
    assert!(state.triple_click_pending);
}

#[test]
fn expire_click_timer_preserves_state_when_deadline_not_yet_reached() {
    let now = Instant::now();
    let mut state = super::ClientMouseState {
        click_deadline: Some(now + std::time::Duration::from_secs(10)),
        double_click_pending: true,
        ..Default::default()
    };
    let result = state.expire_click_timer(now, &layout());
    assert!(result.is_none());
    assert!(state.double_click_pending, "state must be untouched");
    assert!(state.click_deadline.is_some());
}

#[test]
fn drag_end_preserves_modifier_bits_from_release_event() {
    let mut state = super::ClientMouseState {
        drag_flag: 1, // button 0 (drag_flag = mouse_buttons(b) + 1)
        ..Default::default()
    };
    let now = Instant::now();
    let layout = layout();
    // Release with Ctrl held (bit 16 = MOUSE_MASK_CTRL)
    let release = super::MouseForwardEvent {
        b: 3,   // release
        lb: 16, // last button was button 0 + Ctrl (MOUSE_MASK_CTRL)
        x: 5,
        y: 5,
        lx: 5,
        ly: 5,
        sgr_b: 16,
        sgr_type: 'm',
        ignore: false,
    };
    let event = classify_mouse_event(&mut state, &layout, release, now).expect("drag end");
    // Key should be MouseDragEnd1Pane with CTRL modifier
    let base_key = key_string_lookup_string("MouseDragEnd1Pane").unwrap();
    assert_eq!(event.key & !rmux_core::KEYC_CTRL, base_key);
    assert_ne!(
        event.key & rmux_core::KEYC_CTRL,
        0,
        "Ctrl modifier preserved"
    );
}

#[test]
fn sgr_release_provides_correct_button_for_mouse_up() {
    let mut state = super::ClientMouseState::default();
    let now = Instant::now();
    let layout = layout();
    // SGR release for button 2 (middle)
    let release = super::MouseForwardEvent {
        b: 3,  // release (legacy encoding)
        lb: 1, // last was button 2
        x: 5,
        y: 5,
        lx: 5,
        ly: 5,
        sgr_b: 1, // button 2 (SGR preserves button identity)
        sgr_type: 'm',
        ignore: false,
    };
    let event = classify_mouse_event(&mut state, &layout, release, now).expect("mouse up");
    assert_eq!(
        event.key,
        key_string_lookup_string("MouseUp2Pane").unwrap(),
        "SGR release should use sgr_b for button identity"
    );
}

#[test]
fn wheel_events_are_not_affected_by_active_drag() {
    let mut state = super::ClientMouseState {
        drag_flag: 1,
        ..Default::default()
    };
    let now = Instant::now();
    let layout = layout();
    // Wheel up during an active drag
    let wheel = raw(64, 5, 5);
    let event = classify_mouse_event(&mut state, &layout, wheel, now).expect("wheel");
    assert_eq!(
        event.key,
        key_string_lookup_string("WheelUpPane").unwrap(),
        "wheel event is not converted to drag end"
    );
    assert_eq!(
        state.drag_flag, 1,
        "drag flag preserved across wheel events"
    );
}

#[test]
fn scrollbar_from_view_with_zero_history_returns_full_slider() {
    let sb = PaneScrollbar::from_view(
        10,
        0,
        false,
        PaneScrollbarsMode::On,
        ScrollbarPosition::Right,
        1,
        0,
        None,
    )
    .expect("scrollbar");
    assert_eq!(sb.slider_y, 0);
    assert_eq!(
        sb.slider_h, 10,
        "slider covers entire scrollbar when no history"
    );
}

#[test]
fn scrollbar_from_view_alternate_on_returns_none() {
    assert!(PaneScrollbar::from_view(
        10,
        100,
        true,
        PaneScrollbarsMode::On,
        ScrollbarPosition::Right,
        1,
        0,
        None,
    )
    .is_none());
}

#[test]
fn scrollbar_from_view_modal_without_copy_mode_returns_none() {
    assert!(PaneScrollbar::from_view(
        10,
        100,
        false,
        PaneScrollbarsMode::Modal,
        ScrollbarPosition::Right,
        1,
        0,
        None,
    )
    .is_none());
}

#[test]
fn scrollbar_from_view_modal_with_copy_mode_returns_some() {
    assert!(PaneScrollbar::from_view(
        10,
        100,
        false,
        PaneScrollbarsMode::Modal,
        ScrollbarPosition::Right,
        1,
        0,
        Some(0),
    )
    .is_some());
}

#[test]
fn scrollbar_from_view_zero_rows_returns_none() {
    assert!(PaneScrollbar::from_view(
        0,
        100,
        false,
        PaneScrollbarsMode::On,
        ScrollbarPosition::Right,
        1,
        0,
        None,
    )
    .is_none());
}

#[test]
fn scrollbar_from_view_zero_width_returns_none() {
    assert!(PaneScrollbar::from_view(
        10,
        100,
        false,
        PaneScrollbarsMode::On,
        ScrollbarPosition::Right,
        0,
        0,
        None,
    )
    .is_none());
}

#[test]
fn scrollbar_slider_never_exceeds_scrollbar_height() {
    // Large history with small viewport: slider should still fit
    let sb = PaneScrollbar::from_view(
        5,
        10000,
        false,
        PaneScrollbarsMode::On,
        ScrollbarPosition::Right,
        1,
        0,
        Some(5000),
    )
    .expect("scrollbar");
    assert!(sb.slider_y < 5, "slider_y within bounds");
    assert!(sb.slider_h >= 1, "slider_h at least 1");
    assert!(
        sb.slider_y + sb.slider_h <= 5,
        "slider bottom within scrollbar: {} + {} <= 5",
        sb.slider_y,
        sb.slider_h
    );
}

#[test]
fn scrollbar_copy_mode_offset_at_max_produces_slider_at_top() {
    let sb = PaneScrollbar::from_view(
        10,
        100,
        false,
        PaneScrollbarsMode::On,
        ScrollbarPosition::Right,
        1,
        0,
        Some(100),
    )
    .expect("scrollbar");
    // At max offset, slider should be near top
    assert!(sb.slider_y <= 10);
    assert!(sb.slider_h >= 1);
    assert!(
        sb.slider_y + sb.slider_h <= 10,
        "slider bottom within scrollbar at max offset: {} + {} <= 10",
        sb.slider_y,
        sb.slider_h
    );
}

#[test]
fn scrollbar_slider_clamped_at_extreme_offsets() {
    // Test with offset that would push slider_y to the very bottom
    for offset in [0, 1, 50, 99, 100, 200, 500, 1000] {
        let sb = PaneScrollbar::from_view(
            3,
            1000,
            false,
            PaneScrollbarsMode::On,
            ScrollbarPosition::Right,
            1,
            0,
            Some(offset),
        )
        .expect("scrollbar");
        assert!(
            sb.slider_y + sb.slider_h <= 3,
            "offset {offset}: slider bottom within scrollbar: {} + {} <= 3",
            sb.slider_y,
            sb.slider_h
        );
    }
}

#[test]
fn click_sequence_resets_on_different_button() {
    let mut state = super::ClientMouseState::default();
    let base = Instant::now();
    let layout = layout();

    // First click with button 0
    let _ = classify_mouse_event(&mut state, &layout, raw(0, 5, 5), base);
    // Second click with button 1 (different button)
    let second = classify_mouse_event(
        &mut state,
        &layout,
        super::MouseForwardEvent {
            lx: 5,
            ly: 5,
            ..raw(1, 5, 5)
        },
        base + std::time::Duration::from_millis(50),
    )
    .expect("reset to down");
    assert_eq!(
        second.key,
        key_string_lookup_string("MouseDown2Pane").unwrap(),
        "different button resets to MouseDown"
    );
}

#[test]
fn mouse_move_outside_active_pane_does_not_trigger_focus_change() {
    let mut state = super::ClientMouseState::default();
    let mut layout = layout();
    layout.focus_follows_mouse = true;
    layout.active_pane = Some(PaneId::new(0));
    // Move within the active pane
    let event = classify_mouse_event(
        &mut state,
        &layout,
        super::MouseForwardEvent {
            b: 35,
            lb: 35,
            x: 5,
            y: 5,
            lx: 4,
            ly: 5,
            sgr_b: 35,
            sgr_type: 'm',
            ignore: false,
        },
        Instant::now(),
    )
    .expect("move");
    assert_eq!(
        event.focus_target, None,
        "no focus change within active pane"
    );
}

#[test]
fn nowhere_hit_returns_none() {
    let mut state = super::ClientMouseState::default();
    let mut layout = layout();
    layout.panes.clear(); // no panes to hit
                          // Click at coordinates that don't match any pane
    let result = classify_mouse_event(&mut state, &layout, raw(0, 5, 5), Instant::now());
    assert!(result.is_none(), "nowhere hit returns None");
}

#[test]
fn left_scrollbar_position_hit_detection() {
    let mut state = super::ClientMouseState::default();
    let mut layout = layout();
    layout.panes[0].scrollbar = Some(PaneScrollbar {
        position: ScrollbarPosition::Left,
        width: 1,
        pad: 0,
        slider_y: 3,
        slider_h: 4,
    });
    layout.panes[0].geometry = PaneGeometry::new(1, 0, 39, 10);
    // Click on the left scrollbar column (x=0)
    let event = classify_mouse_event(&mut state, &layout, raw(0, 0, 2), Instant::now())
        .expect("scrollbar up");
    assert_eq!(event.event.location, MouseLocation::ScrollbarUp);
}

#[test]
fn copy_mode_mouse_context_returns_none_without_pane_id() {
    let event = super::AttachedMouseEvent {
        raw: raw(0, 5, 5),
        session_id: 1,
        window_id: Some(5),
        pane_id: None,
        pane_target: None,
        location: MouseLocation::Pane,
        status_at: None,
        status_lines: 0,
        ignore: false,
    };
    assert!(copy_mode_mouse_context(&event, PaneGeometry::new(0, 0, 40, 10), 0).is_none());
}
