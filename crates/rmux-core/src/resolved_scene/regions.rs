use super::*;

pub(super) fn hit_regions_for_pane(
    content: SignedPaneRect,
    raw_content_clip: SignedPaneRect,
    frame: SignedPaneRect,
    context: ResolvedSceneContext,
) -> Vec<ResolvedHitRegion> {
    // Keep the legacy viewport-edge placement, but retain the source width.
    let status = pane_border_status_rect(raw_content_clip, context).map(|rect| SignedPaneRect {
        x: content.x,
        cols: content.cols,
        ..rect
    });
    let mut regions = Vec::new();
    let mut push = |kind, rect: SignedPaneRect| {
        regions.push(ResolvedHitRegion {
            kind,
            rect,
            clip: rect.clip_to(context.content_size),
        });
    };
    for rect in subtract_status(content, status) {
        push(ResolvedHitRegionKind::Content, rect);
    }
    // A frame is a ring, not its bounding rectangle. Subtract status cells as
    // well so a consumer needs neither ordering nor a special exclusion rule.
    for edge in subtract(frame, content) {
        for rect in subtract_status(edge, status) {
            push(ResolvedHitRegionKind::Frame, rect);
        }
    }
    if let Some(rect) = status {
        push(ResolvedHitRegionKind::PaneBorderStatus, rect);
    }
    regions
}

fn subtract_status(rect: SignedPaneRect, status: Option<SignedPaneRect>) -> Vec<SignedPaneRect> {
    status.map_or_else(|| vec![rect], |status| subtract(rect, status))
}

/// Disjoint rectangular pieces of `source` outside `cut`, before viewport clipping.
fn subtract(source: SignedPaneRect, cut: SignedPaneRect) -> Vec<SignedPaneRect> {
    let right = source.x.saturating_add(source.cols);
    let bottom = source.y.saturating_add(source.rows);
    let left_cut = source.x.max(cut.x);
    let top_cut = source.y.max(cut.y);
    let right_cut = right.min(cut.x.saturating_add(cut.cols));
    let bottom_cut = bottom.min(cut.y.saturating_add(cut.rows));
    if left_cut >= right_cut || top_cut >= bottom_cut {
        return vec![source];
    }
    [
        (
            source.x,
            source.y,
            source.cols,
            top_cut.saturating_sub(source.y),
        ),
        (
            source.x,
            bottom_cut,
            source.cols,
            bottom.saturating_sub(bottom_cut),
        ),
        (
            source.x,
            top_cut,
            left_cut.saturating_sub(source.x),
            bottom_cut.saturating_sub(top_cut),
        ),
        (
            right_cut,
            top_cut,
            right.saturating_sub(right_cut),
            bottom_cut.saturating_sub(top_cut),
        ),
    ]
    .into_iter()
    .filter_map(|(x, y, cols, rows)| {
        (cols > 0 && rows > 0).then_some(SignedPaneRect { x, y, cols, rows })
    })
    .collect()
}
