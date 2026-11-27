use rmux_proto::{SplitDirection, TerminalSize};

use super::*;

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize { cols, rows }
}

fn context(
    size: TerminalSize,
    pane_border_status: ResolvedPaneBorderStatus,
    zoom_visibility: ResolvedZoomVisibility,
) -> ResolvedSceneContext {
    ResolvedSceneContext::new(size, pane_border_status, zoom_visibility)
}

fn rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

#[test]
fn resolves_stable_identity_focus_layers_and_signed_clips() {
    let mut window = Window::new(size(80, 24));
    window.split_after_position_with_id_and_direction(0, PaneId::new(10), SplitDirection::Vertical);
    window.float_pane(1, rect(-3, 2, 20, 8)).unwrap();
    window.select_floating_pane(1).unwrap();

    let scene = window.resolved_scene(context(
        size(15, 6),
        ResolvedPaneBorderStatus::Bottom,
        ResolvedZoomVisibility::Rendered,
    ));

    assert_eq!(scene.window_id(), window.id());
    assert_eq!(scene.active_pane_id(), Some(PaneId::new(10)));
    assert_eq!(scene.panes().len(), 2);
    let tiled = scene.pane_by_index(0).expect("tiled pane");
    assert_eq!(tiled.layer(), ResolvedPaneLayer::Tiled);
    assert!(!tiled.focused());
    let floating = scene.pane_by_id(PaneId::new(10)).expect("floating pane");
    assert_eq!(floating.layer(), ResolvedPaneLayer::Floating(1));
    assert!(floating.focused());
    assert_eq!(
        floating.content(),
        SignedPaneRect::new(-3, 2, 20, 8).unwrap()
    );
    assert_eq!(
        floating.content_clip(),
        SignedPaneRect::new(0, 2, 15, 3).unwrap()
    );
    assert_eq!(
        floating.frame(),
        SignedPaneRect::new(-4, 1, 22, 10).unwrap()
    );
    assert_eq!(
        floating.frame_clip(),
        SignedPaneRect::new(0, 1, 15, 5).unwrap()
    );
    assert!(floating
        .hit_regions()
        .iter()
        .any(|region| region.kind() == ResolvedHitRegionKind::PaneBorderStatus));
}

#[test]
fn render_visibility_hides_zoom_siblings_without_losing_layout_projection() {
    let mut window = Window::new(size(80, 24));
    window.split_after_position_with_id_and_direction(0, PaneId::new(10), SplitDirection::Vertical);
    window.toggle_zoom(1);

    let rendered = window.resolved_scene(context(
        size(80, 20),
        ResolvedPaneBorderStatus::Off,
        ResolvedZoomVisibility::Rendered,
    ));
    assert!(!rendered.pane_by_index(0).expect("hidden sibling").visible());
    let active = rendered.pane_by_index(1).expect("active pane");
    assert!(active.visible());
    assert_eq!(
        active.content_clip_geometry(),
        PaneGeometry::new(0, 0, 80, 20)
    );

    let layout = window.resolved_scene(context(
        size(80, 20),
        ResolvedPaneBorderStatus::Off,
        ResolvedZoomVisibility::IncludeHidden,
    ));
    assert!(layout.pane_by_index(0).expect("layout sibling").visible());
}

#[test]
fn same_window_and_context_resolve_the_same_scene() {
    let mut window = Window::new(size(40, 10));
    window.split_after_position_with_id_and_direction(
        0,
        PaneId::new(10),
        SplitDirection::Horizontal,
    );
    let context = context(
        size(40, 9),
        ResolvedPaneBorderStatus::Top,
        ResolvedZoomVisibility::Rendered,
    );

    assert_eq!(
        window.resolved_scene(context),
        window.resolved_scene(context)
    );
}

fn contains(rect: SignedPaneRect, x: i32, y: i32) -> bool {
    x >= rect.x()
        && x < rect.x().saturating_add(rect.cols())
        && y >= rect.y()
        && y < rect.y().saturating_add(rect.rows())
}

#[test]
fn hit_regions_partition_visible_cells_and_preserve_sources() {
    for placement in [
        rect(-3, 2, 20, 8),
        rect(3, -2, 6, 8),
        rect(3, 2, 6, 2),
        rect(0, 0, 15, 6),
    ] {
        for status in [
            ResolvedPaneBorderStatus::Off,
            ResolvedPaneBorderStatus::Top,
            ResolvedPaneBorderStatus::Bottom,
        ] {
            let mut window = Window::new(size(30, 12));
            window.float_pane(0, placement).unwrap();
            let scene = window.resolved_scene(context(
                size(15, 6),
                status,
                ResolvedZoomVisibility::Rendered,
            ));
            let pane = scene.pane_by_index(0).unwrap();
            for region in pane.hit_regions() {
                assert_eq!(region.clip(), region.rect().clip_to(size(15, 6)));
                if region.kind() == ResolvedHitRegionKind::PaneBorderStatus {
                    assert_eq!(region.rect().x(), placement.x());
                    assert_eq!(region.rect().cols(), placement.cols());
                }
            }
            for y in 0..6 {
                for x in 0..15 {
                    let hits: Vec<_> = pane
                        .hit_regions()
                        .iter()
                        .filter(|r| contains(r.clip(), x, y))
                        .collect();
                    assert_eq!(
                        hits.len(),
                        usize::from(contains(pane.frame_clip(), x, y)),
                        "{placement:?} {status:?} at {x},{y}: {hits:?}"
                    );
                    if contains(pane.content_clip(), x, y) {
                        assert_eq!(hits[0].kind(), ResolvedHitRegionKind::Content);
                    }
                }
            }
        }
    }
}

#[test]
fn status_source_retains_unclipped_horizontal_extent() {
    let mut window = Window::new(size(80, 24));
    window.float_pane(0, rect(-3, 2, 20, 8)).unwrap();
    let scene = window.resolved_scene(context(
        size(15, 6),
        ResolvedPaneBorderStatus::Bottom,
        ResolvedZoomVisibility::Rendered,
    ));
    let status = scene.panes()[0]
        .hit_regions()
        .iter()
        .find(|r| r.kind() == ResolvedHitRegionKind::PaneBorderStatus)
        .unwrap();
    assert_eq!(status.rect(), SignedPaneRect::new(-3, 5, 20, 1).unwrap());
    assert_eq!(status.clip(), SignedPaneRect::new(0, 5, 15, 1).unwrap());
}

#[test]
fn legacy_border_helpers_keep_viewport_edge_and_empty_behavior() {
    for (source, bounds) in [
        (SignedPaneRect::new(-3, 2, 20, 8).unwrap(), size(15, 6)),
        (SignedPaneRect::new(2, -3, 4, 8).unwrap(), size(15, 6)),
        (SignedPaneRect::new(2, 2, 4, 2).unwrap(), size(15, 6)),
        (SignedPaneRect::new(-20, -20, 2, 2).unwrap(), size(15, 6)),
        (SignedPaneRect::new(20, 20, 2, 2).unwrap(), size(15, 6)),
        (SignedPaneRect::new(0, 0, 2, 2).unwrap(), size(0, 0)),
        (SignedPaneRect::new(0, 0, 2, 2).unwrap(), size(1, 1)),
    ] {
        for status in [
            ResolvedPaneBorderStatus::Off,
            ResolvedPaneBorderStatus::Top,
            ResolvedPaneBorderStatus::Bottom,
        ] {
            let ctx = context(bounds, status, ResolvedZoomVisibility::Rendered);
            let raw = source.clip_to(bounds);
            let legacy_status = pane_border_status_rect(raw, ctx);
            let legacy_content = content_clip_for_border_status(raw, ctx);
            let regions = regions::hit_regions_for_pane(source, raw, source.frame(), ctx);
            let actual_status = regions
                .iter()
                .find(|r| r.kind() == ResolvedHitRegionKind::PaneBorderStatus);
            assert_eq!(
                actual_status.map(|r| r.clip()),
                legacy_status.map(|r| r.clip_to(bounds))
            );
            for y in 0..i32::from(bounds.rows) {
                for x in 0..i32::from(bounds.cols) {
                    assert_eq!(
                        regions
                            .iter()
                            .any(|r| r.kind() == ResolvedHitRegionKind::Content
                                && contains(r.clip(), x, y)),
                        contains(legacy_content, x, y)
                    );
                }
            }
        }
    }
}

#[test]
fn public_pane_mut_scene_uses_occupied_cell_without_resealing() {
    use crate::SessionStore;
    use rmux_proto::{SessionName, Target};

    let mut left = SessionStore::new();
    let mut right = SessionStore::new();
    left.create_session(SessionName::new("test").unwrap(), size(80, 24))
        .unwrap();
    right
        .create_session(SessionName::new("test").unwrap(), size(40, 10))
        .unwrap();
    let target = Target::parse("test").unwrap();
    let session = right.resolve_session_mut(&target).unwrap();
    session.split_active_pane().unwrap();
    session.kill_pane(0).unwrap();
    std::mem::swap(
        left.resolve_session_mut(&target)
            .unwrap()
            .window_at_mut(0)
            .unwrap()
            .pane_mut(0)
            .unwrap(),
        right
            .resolve_session_mut(&target)
            .unwrap()
            .window_at_mut(0)
            .unwrap()
            .pane_mut(0)
            .unwrap(),
    );
    for (store, occupied, donor) in [
        (&left, size(80, 24), size(40, 10)),
        (&right, size(40, 10), size(80, 24)),
    ] {
        let window = store.resolve_window(&target).unwrap();
        let id = window.pane(0).unwrap().id();
        assert!(window.layout_dump().ends_with(&format!(
            ",{}x{},0,0,{}",
            occupied.cols,
            occupied.rows,
            id.as_u32()
        )));
        let before = format!("{window:?}");
        let ctx = context(
            occupied,
            ResolvedPaneBorderStatus::Off,
            ResolvedZoomVisibility::Rendered,
        );
        let scene = window.resolved_scene(ctx);
        assert_eq!(
            scene.pane_by_id(id).unwrap().content_clip_geometry(),
            PaneGeometry::new(0, 0, occupied.cols, occupied.rows)
        );
        assert_eq!(
            window.pane(0).unwrap().geometry(),
            PaneGeometry::new(0, 0, donor.cols, donor.rows)
        );
        assert_eq!(format!("{window:?}"), before);
        assert_eq!(window.clone().resolved_scene(ctx), scene);
        let mut restored = window.clone();
        restored.restore_checkpoint(window.checkpoint());
        assert_eq!(restored.resolved_scene(ctx), scene);
    }
}

#[test]
fn remapped_scene_cells_preserve_offsets_floats_zoom_and_context() {
    for floating in [false, true] {
        for zoomed in [false, true] {
            let mut window = Window::new(size(80, 24));
            window.split_after_position_with_id_and_direction(
                0,
                PaneId::new(10),
                SplitDirection::Vertical,
            );
            if floating {
                window.float_pane(1, rect(-3, 2, 20, 8)).unwrap();
            }
            if zoomed {
                window.toggle_zoom(0);
            }
            let mut first = window.pane(0).unwrap().clone();
            let mut second = window.pane(1).unwrap().clone();
            first.set_index(1);
            second.set_index(0);
            let contexts: Vec<_> = [
                ResolvedPaneBorderStatus::Off,
                ResolvedPaneBorderStatus::Top,
                ResolvedPaneBorderStatus::Bottom,
            ]
            .into_iter()
            .flat_map(|border| {
                [
                    ResolvedZoomVisibility::Rendered,
                    ResolvedZoomVisibility::IncludeHidden,
                ]
                .into_iter()
                .map(move |visibility| context(size(60, 18), border, visibility))
            })
            .collect();
            let expected: Vec<_> = contexts
                .iter()
                .map(|ctx| window.resolved_scene(*ctx))
                .collect();
            *window.pane_mut(0).unwrap() = second;
            *window.pane_mut(1).unwrap() = first;
            let before = format!("{window:?}");
            for (ctx, mut expected) in contexts.into_iter().zip(expected) {
                for pane in &mut expected.panes {
                    pane.pane_id = if pane.pane_index == 0 {
                        PaneId::new(10)
                    } else {
                        PaneId::new(0)
                    };
                }
                expected.active_pane_id = Some(window.active_pane().unwrap().id());
                assert_eq!(
                    window.resolved_scene(ctx),
                    expected,
                    "floating={floating}, zoomed={zoomed}"
                );
                assert_eq!(window.clone().resolved_scene(ctx), expected);
                let mut restored = window.clone();
                restored.restore_checkpoint(window.checkpoint());
                assert_eq!(restored.resolved_scene(ctx), expected);
            }
            assert_eq!(format!("{window:?}"), before);
        }
    }
}

#[test]
fn tiled_only_regions_preserve_identity_and_content_for_each_status() {
    let window = Window::new(size(15, 6));
    for status in [
        ResolvedPaneBorderStatus::Off,
        ResolvedPaneBorderStatus::Top,
        ResolvedPaneBorderStatus::Bottom,
    ] {
        let scene = window.resolved_scene(context(
            size(15, 6),
            status,
            ResolvedZoomVisibility::Rendered,
        ));
        let pane = &scene.panes()[0];
        assert_eq!(pane.layer(), ResolvedPaneLayer::Tiled);
        assert_eq!(Some(pane.pane_id()), scene.active_pane_id());
        assert!(pane.visible());
        for y in 0..6 {
            for x in 0..15 {
                let hits: Vec<_> = pane
                    .hit_regions()
                    .iter()
                    .filter(|r| contains(r.clip(), x, y))
                    .collect();
                assert_eq!(hits.len(), 1);
                assert_eq!(
                    hits[0].kind(),
                    if contains(pane.content_clip(), x, y) {
                        ResolvedHitRegionKind::Content
                    } else {
                        ResolvedHitRegionKind::PaneBorderStatus
                    }
                );
            }
        }
    }
}

#[test]
fn all_floating_scene_needs_no_tiled_tree_after_public_replacement() {
    let mut window = Window::new(size(80, 24));
    window.float_pane(0, rect(-4, 3, 20, 8)).unwrap();
    *window.pane_mut(0).unwrap() =
        Pane::new_with_id(PaneId::new(99), 0, PaneGeometry::new(0, 0, 1, 1));
    let ctx = context(
        size(80, 24),
        ResolvedPaneBorderStatus::Off,
        ResolvedZoomVisibility::Rendered,
    );
    let before = format!("{window:?}");
    let scene = window.resolved_scene(ctx);
    assert_eq!(scene.panes().len(), 1);
    assert_eq!(
        scene.pane_by_id(PaneId::new(99)).unwrap().content(),
        SignedPaneRect::new(-4, 3, 20, 8).unwrap()
    );
    assert_eq!(format!("{window:?}"), before);
}
