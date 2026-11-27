//! Clipping and occlusion of signed scene rectangles into the visible runs of each terminal row.

use rmux_core::{ResolvedHitRegionKind, ResolvedPaneScene, SignedPaneRect};

use super::super::pane_scrollbar::RenderedPaneScrollbarRun;
use super::TerminalPaneSpan;

pub(super) fn pane_content_source(scene: &ResolvedPaneScene) -> Option<SignedPaneRect> {
    scene
        .hit_regions()
        .iter()
        .filter(|region| region.kind() == ResolvedHitRegionKind::Content)
        .map(|region| region.rect())
        .reduce(union)
        .or_else(|| Some(scene.content()))
}

pub(super) fn visible_spans(
    source: SignedPaneRect,
    clip: SignedPaneRect,
    occluders: &[SignedPaneRect],
    y_offset: u16,
) -> Vec<TerminalPaneSpan> {
    let row_start = usize::try_from(clip.y().saturating_sub(source.y())).unwrap_or(usize::MAX);
    (0..clip.rows())
        .flat_map(|line| {
            let y = clip.y().saturating_add(line);
            visible_runs_for_row(source, clip, y, occluders)
                .into_iter()
                .filter_map(move |run| {
                    Some(TerminalPaneSpan {
                        x: run.x,
                        y: run.y_for(y, y_offset)?,
                        source_row: row_start.checked_add(usize::try_from(line).ok()?)?,
                        source_col: usize::try_from(i32::from(run.x).saturating_sub(source.x()))
                            .ok()?,
                        line_index: usize::try_from(line).ok()?,
                        cols: run.width,
                    })
                })
        })
        .collect()
}

pub(in crate::renderer) fn visible_runs_for_row(
    source: SignedPaneRect,
    clip: SignedPaneRect,
    y: i32,
    occluders: &[SignedPaneRect],
) -> Vec<RenderedPaneScrollbarRun> {
    if y < clip.y() || y >= clip.y().saturating_add(clip.rows()) {
        return Vec::new();
    }
    let mut intervals = vec![(clip.x(), clip.x().saturating_add(clip.cols()))];
    for occluder in occluders {
        if y < occluder.y() || y >= occluder.y().saturating_add(occluder.rows()) {
            continue;
        }
        let cut = (occluder.x(), occluder.x().saturating_add(occluder.cols()));
        intervals = subtract_intervals(&intervals, cut);
    }
    intervals
        .into_iter()
        .filter_map(|(start, end)| {
            let start = start.max(source.x());
            let end = end.min(source.x().saturating_add(source.cols()));
            (start < end).then_some((start, end))
        })
        .filter_map(|(start, end)| {
            Some(RenderedPaneScrollbarRun {
                x: u16::try_from(start).ok()?,
                width: u16::try_from(end.saturating_sub(start)).ok()?,
            })
        })
        .collect()
}

fn subtract_intervals(intervals: &[(i32, i32)], cut: (i32, i32)) -> Vec<(i32, i32)> {
    intervals
        .iter()
        .flat_map(|&(start, end)| {
            if cut.1 <= start || cut.0 >= end {
                return vec![(start, end)];
            }
            let mut pieces = Vec::with_capacity(2);
            if start < cut.0 {
                pieces.push((start, cut.0.min(end)));
            }
            if cut.1 < end {
                pieces.push((cut.1.max(start), end));
            }
            pieces
        })
        .filter(|(start, end)| start < end)
        .collect()
}

fn union(a: SignedPaneRect, b: SignedPaneRect) -> SignedPaneRect {
    let x = a.x().min(b.x());
    let y = a.y().min(b.y());
    SignedPaneRect::new(
        x,
        y,
        a.x()
            .saturating_add(a.cols())
            .max(b.x().saturating_add(b.cols()))
            .saturating_sub(x),
        a.y()
            .saturating_add(a.rows())
            .max(b.y().saturating_add(b.rows()))
            .saturating_sub(y),
    )
    .expect("union dimensions are non-negative")
}

pub(super) fn intersection(a: SignedPaneRect, b: SignedPaneRect) -> SignedPaneRect {
    let x = a.x().max(b.x());
    let y = a.y().max(b.y());
    SignedPaneRect::new(
        x,
        y,
        a.x()
            .saturating_add(a.cols())
            .min(b.x().saturating_add(b.cols()))
            .saturating_sub(x)
            .max(0),
        a.y()
            .saturating_add(a.rows())
            .min(b.y().saturating_add(b.rows()))
            .saturating_sub(y)
            .max(0),
    )
    .expect("intersection dimensions are non-negative")
}

pub(super) fn contains(rect: SignedPaneRect, x: i32, y: i32) -> bool {
    x >= rect.x()
        && y >= rect.y()
        && x < rect.x().saturating_add(rect.cols())
        && y < rect.y().saturating_add(rect.rows())
}
