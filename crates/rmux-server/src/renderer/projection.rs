mod occlusion;
mod scene;
mod scrollbar;
pub(super) use occlusion::visible_runs_for_row;
use occlusion::{contains, intersection, pane_content_source, visible_spans};
pub(crate) use scene::window_scene;

use crate::pane_scrollbar::PaneScrollbarConfig;
use rmux_core::{
    OptionStore, Pane, PaneGeometry, PaneId, ResolvedHitRegionKind, ResolvedPaneLayer,
    ResolvedPaneScene, ResolvedScene, SignedPaneRect,
};

use super::pane_scrollbar::{RenderedPaneScrollbar, RenderedPaneScrollbarRun};
use super::StatusGeometry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalSceneProjection {
    geometry: StatusGeometry,
    scene: ResolvedScene,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalPaneProjection {
    source: SignedPaneRect,
    clip: SignedPaneRect,
    spans: Vec<TerminalPaneSpan>,
    scrollbar: Option<RenderedPaneScrollbar>,
    y_offset: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalPaneSpan {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) source_row: usize,
    pub(super) source_col: usize,
    pub(super) line_index: usize,
    pub(super) cols: u16,
}

/// A horizontal stretch of terminal cells no visible pane draws on.
///
/// Runs rather than cells because the background is the one layer that covers
/// whole rows at a time: a margin is addressed once and then repeated, instead
/// of re-addressing every column the way border cells must.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutsideRun {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) cols: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalBorderCell {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) glyph: char,
    pub(super) pane_index: u32,
    pub(super) active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalStatusRegion {
    source: SignedPaneRect,
    clip: SignedPaneRect,
    spans: Vec<TerminalPaneSpan>,
    y_offset: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProjectedPaneScrollbarContext {
    pub(super) history_size: usize,
    pub(super) alternate_on: bool,
    pub(super) copy_mode_scroll_position: Option<usize>,
}

impl TerminalSceneProjection {
    pub(super) fn resolve(
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
    ) -> Self {
        let geometry = StatusGeometry::for_session(session, options);
        let scene = crate::pane_visible_geometry::resolved_session_scene(
            session,
            options,
            geometry.content_size(),
        );
        Self { geometry, scene }
    }

    pub(super) const fn geometry(&self) -> StatusGeometry {
        self.geometry
    }

    pub(super) fn has_floating_panes(&self) -> bool {
        self.scene
            .panes()
            .iter()
            .any(|pane| pane.layer() != ResolvedPaneLayer::Tiled)
    }

    pub(super) fn pane_ids_rear_to_front(&self) -> Vec<PaneId> {
        self.scene
            .panes()
            .iter()
            .map(ResolvedPaneScene::pane_id)
            .collect()
    }

    pub(super) fn pane_projection(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane: &Pane,
        context: ProjectedPaneScrollbarContext,
    ) -> Option<TerminalPaneProjection> {
        let config = PaneScrollbarConfig::resolve(
            options,
            session.name(),
            session.active_window_index(),
            pane.index(),
        );
        self.pane_projection_with_config(pane.id(), pane.index(), &config, context)
    }

    pub(super) fn pane_projection_with_config(
        &self,
        pane_id: PaneId,
        pane_index: u32,
        config: &PaneScrollbarConfig,
        context: ProjectedPaneScrollbarContext,
    ) -> Option<TerminalPaneProjection> {
        let pane_scene = self
            .scene
            .pane_by_id(pane_id)
            .or_else(|| self.scene.pane_by_index(pane_index))?;
        if !pane_scene.visible() {
            return None;
        }

        let raw_source = pane_content_source(pane_scene)?;
        let raw_clip = intersection(raw_source, pane_scene.content_clip());
        let occluders = self.front_occluders(pane_scene.layer());
        let (source, clip, scrollbar) =
            self.project_scrollbar(raw_source, raw_clip, config, context, &occluders)?;
        let spans = visible_spans(source, clip, &occluders, self.geometry.content_y_offset);
        Some(TerminalPaneProjection {
            source,
            clip,
            spans,
            scrollbar,
            y_offset: self.geometry.content_y_offset,
        })
    }

    pub(super) fn raw_pane_projection(&self, pane: &Pane) -> Option<TerminalStatusRegion> {
        let pane_scene = self
            .scene
            .pane_by_id(pane.id())
            .or_else(|| self.scene.pane_by_index(pane.index()))?;
        if !pane_scene.visible() {
            return None;
        }
        let source = pane_content_source(pane_scene)?;
        let clip = intersection(source, pane_scene.content_clip());
        let spans = visible_spans(
            source,
            clip,
            &self.front_occluders(pane_scene.layer()),
            self.geometry.content_y_offset,
        );
        Some(TerminalStatusRegion {
            source,
            clip,
            spans,
            y_offset: self.geometry.content_y_offset,
        })
    }

    pub(super) fn pane_border_status_region(&self, pane: &Pane) -> Option<TerminalStatusRegion> {
        let pane_scene = self
            .scene
            .pane_by_id(pane.id())
            .or_else(|| self.scene.pane_by_index(pane.index()))?;
        if !pane_scene.visible() {
            return None;
        }
        let region = pane_scene
            .hit_regions()
            .iter()
            .find(|region| region.kind() == ResolvedHitRegionKind::PaneBorderStatus)?;
        let source = region.rect();
        let clip = intersection(source, region.clip());
        let spans = visible_spans(
            source,
            clip,
            &self.front_occluders(pane_scene.layer()),
            self.geometry.content_y_offset,
        );
        Some(TerminalStatusRegion {
            source,
            clip,
            spans,
            y_offset: self.geometry.content_y_offset,
        })
    }

    pub(super) fn tiled_content_geometries(&self) -> Vec<(u32, PaneGeometry)> {
        self.scene
            .panes()
            .iter()
            .filter(|pane| pane.visible() && pane.layer() == ResolvedPaneLayer::Tiled)
            .filter_map(|pane| {
                let geometry = pane.content_clip_geometry();
                (geometry.cols() != 0 && geometry.rows() != 0)
                    .then_some((pane.pane_index(), geometry))
            })
            .collect()
    }

    pub(super) fn cell_visible_at_layer(&self, x: u16, y: u16, layer: ResolvedPaneLayer) -> bool {
        !self
            .front_occluders(layer)
            .iter()
            .any(|occluder| contains(*occluder, i32::from(x), i32::from(y)))
    }

    /// The cells of the physical client terminal that no visible pane draws on.
    ///
    /// The surface is the terminal itself, not the window: a window sized
    /// smaller than its client leaves margins, and those margins are the
    /// point. Everything a pane owns is subtracted — its content, the frame
    /// ring around it and its pane-border-status row — so a drawn border, a
    /// scrollbar (which lives inside the pane's own content rectangle) and an
    /// empty application area all keep their own rendering. A pane that is not
    /// visible claims nothing, which is what turns the gap a hidden or moved
    /// float leaves behind into an outside region rather than a hole.
    ///
    /// The status lines are excluded here rather than by the caller, because
    /// they are the one band of the terminal that is not window content at
    /// all; `content_y_offset` is applied exactly once, where a scene cell
    /// becomes a terminal cell.
    pub(super) fn outside_runs(&self) -> Vec<OutsideRun> {
        let terminal = self.geometry.terminal_size;
        if terminal.cols == 0 || terminal.rows == 0 {
            return Vec::new();
        }
        let owned = self.owned_terminal_rects();
        let status = self
            .geometry
            .status_y
            .map(|y| (y, y.saturating_add(self.geometry.status_lines)));

        let mut runs = Vec::new();
        let mut row = vec![false; usize::from(terminal.cols)];
        for y in 0..terminal.rows {
            if status.is_some_and(|(first, past_last)| y >= first && y < past_last) {
                continue;
            }
            row.iter_mut().for_each(|cell| *cell = false);
            for rect in &owned {
                if i32::from(y) < rect.y() || i32::from(y) >= rect.y().saturating_add(rect.rows()) {
                    continue;
                }
                for x in 0..terminal.cols {
                    if contains(*rect, i32::from(x), i32::from(y)) {
                        row[usize::from(x)] = true;
                    }
                }
            }
            let mut start: Option<u16> = None;
            for x in 0..terminal.cols {
                if row[usize::from(x)] {
                    if let Some(from) = start.take() {
                        runs.push(OutsideRun {
                            x: from,
                            y,
                            cols: x.saturating_sub(from),
                        });
                    }
                } else if start.is_none() {
                    start = Some(x);
                }
            }
            if let Some(from) = start {
                runs.push(OutsideRun {
                    x: from,
                    y,
                    cols: terminal.cols.saturating_sub(from),
                });
            }
        }
        runs
    }

    /// Every hit region of every visible pane, in terminal coordinates.
    fn owned_terminal_rects(&self) -> Vec<SignedPaneRect> {
        let y_offset = i32::from(self.geometry.content_y_offset);
        self.scene
            .panes()
            .iter()
            .filter(|pane| pane.visible())
            .flat_map(ResolvedPaneScene::hit_regions)
            .filter_map(|region| {
                let clip = region.clip();
                (clip.cols() > 0 && clip.rows() > 0)
                    .then(|| {
                        SignedPaneRect::new(
                            clip.x(),
                            clip.y().saturating_add(y_offset),
                            clip.cols(),
                            clip.rows(),
                        )
                    })
                    .flatten()
            })
            .collect()
    }

    pub(super) fn floating_border_cells(&self) -> Vec<TerminalBorderCell> {
        let mut cells = Vec::new();
        for pane_scene in self
            .scene
            .panes()
            .iter()
            .filter(|pane| pane.visible() && pane.layer() != ResolvedPaneLayer::Tiled)
        {
            let occluders = self.front_occluders(pane_scene.layer());
            for region in pane_scene
                .hit_regions()
                .iter()
                .filter(|region| region.kind() == ResolvedHitRegionKind::Frame)
            {
                let clip = region.clip();
                for scene_y in clip.y()..clip.y().saturating_add(clip.rows()) {
                    for scene_x in clip.x()..clip.x().saturating_add(clip.cols()) {
                        if occluders
                            .iter()
                            .any(|occluder| contains(*occluder, scene_x, scene_y))
                        {
                            continue;
                        }
                        let Some((terminal_x, terminal_y)) =
                            terminal_cell(scene_x, scene_y, self.geometry.content_y_offset)
                        else {
                            continue;
                        };
                        cells.push(TerminalBorderCell {
                            x: terminal_x,
                            y: terminal_y,
                            glyph: floating_frame_glyph(pane_scene.frame(), scene_x, scene_y),
                            pane_index: pane_scene.pane_index(),
                            active: pane_scene.focused(),
                        });
                    }
                }
            }
        }
        cells
    }

    fn project_scrollbar(
        &self,
        raw_source: SignedPaneRect,
        raw_clip: SignedPaneRect,
        config: &PaneScrollbarConfig,
        context: ProjectedPaneScrollbarContext,
        occluders: &[SignedPaneRect],
    ) -> Option<(
        SignedPaneRect,
        SignedPaneRect,
        Option<RenderedPaneScrollbar>,
    )> {
        let local = config.layout(
            PaneGeometry::new(
                0,
                0,
                u16::try_from(raw_source.cols()).unwrap_or(u16::MAX),
                u16::try_from(raw_source.rows()).unwrap_or(u16::MAX),
            ),
            context.alternate_on,
            context.copy_mode_scroll_position.is_some(),
        );
        let source = SignedPaneRect::new(
            raw_source.x().saturating_add(i32::from(local.content.x())),
            raw_source.y().saturating_add(i32::from(local.content.y())),
            i32::from(local.content.cols()),
            i32::from(local.content.rows()),
        )?;
        let clip = intersection(source, raw_clip);
        let scrollbar = scrollbar::projected_scrollbar(
            raw_source,
            raw_clip,
            local,
            context,
            config,
            occluders,
            self.geometry.content_y_offset,
        );
        Some((source, clip, scrollbar))
    }

    fn front_occluders(&self, layer: ResolvedPaneLayer) -> Vec<SignedPaneRect> {
        self.scene
            .panes()
            .iter()
            .filter(|pane| pane.visible() && pane.layer().z_index() > layer.z_index())
            .map(ResolvedPaneScene::frame_clip)
            .filter(|rect| !rect.is_empty())
            .collect()
    }
}

impl TerminalPaneProjection {
    /// Geometry for composing in source-local coordinates before visible-span projection.
    pub(super) fn source_geometry(&self) -> PaneGeometry {
        PaneGeometry::new(
            0,
            0,
            u16::try_from(self.source.cols()).unwrap_or(u16::MAX),
            u16::try_from(self.source.rows()).unwrap_or(u16::MAX),
        )
    }

    pub(super) fn terminal_geometry(&self) -> Option<PaneGeometry> {
        let geometry = self.clip.pane_geometry()?;
        Some(PaneGeometry::new(
            geometry.x(),
            geometry.y().saturating_add(self.y_offset),
            geometry.cols(),
            geometry.rows(),
        ))
    }

    pub(super) const fn source_width(&self) -> u16 {
        if self.source.cols() <= 0 {
            0
        } else if self.source.cols() > u16::MAX as i32 {
            u16::MAX
        } else {
            self.source.cols() as u16
        }
    }

    pub(super) fn source_row_start(&self) -> usize {
        usize::try_from(self.clip.y().saturating_sub(self.source.y())).unwrap_or(usize::MAX)
    }

    pub(super) fn row_count(&self) -> usize {
        usize::try_from(self.clip.rows()).unwrap_or(0)
    }

    pub(super) fn spans(&self) -> &[TerminalPaneSpan] {
        &self.spans
    }

    pub(super) fn scrollbar(&self) -> Option<&RenderedPaneScrollbar> {
        self.scrollbar.as_ref()
    }

    pub(super) fn visible_terminal_cursor(
        &self,
        projected_cursor_x: u16,
        cursor_y: u32,
    ) -> Option<(u16, u16)> {
        if self.source.cols() <= 0 || self.source.rows() <= 0 {
            return None;
        }
        let cursor_y = cursor_y.min(u32::try_from(self.source.rows().saturating_sub(1)).ok()?);
        let scene_x = self
            .source
            .x()
            .saturating_add(i32::from(projected_cursor_x));
        let scene_y = self
            .source
            .y()
            .saturating_add(i32::try_from(cursor_y).ok()?);
        if projected_cursor_x >= self.source_width() || !self.contains_scene_cell(scene_x, scene_y)
        {
            return None;
        }
        terminal_cell(scene_x, scene_y, self.y_offset)
    }

    fn contains_scene_cell(&self, x: i32, y: i32) -> bool {
        self.spans.iter().any(|span| {
            let span_y = i32::from(span.y).saturating_sub(i32::from(self.y_offset));
            let span_x = self
                .source
                .x()
                .saturating_add(i32::try_from(span.source_col).unwrap_or(i32::MAX));
            span_y == y && x >= span_x && x < span_x.saturating_add(i32::from(span.cols))
        })
    }
}

impl TerminalStatusRegion {
    /// Geometry for composing in source-local coordinates before visible-span projection.
    pub(super) fn source_geometry(&self) -> PaneGeometry {
        PaneGeometry::new(
            0,
            0,
            u16::try_from(self.source.cols()).unwrap_or(u16::MAX),
            u16::try_from(self.source.rows()).unwrap_or(u16::MAX),
        )
    }

    pub(super) fn source_width(&self) -> usize {
        usize::try_from(self.source.cols()).unwrap_or(0)
    }

    pub(super) fn spans(&self) -> &[TerminalPaneSpan] {
        &self.spans
    }
}

pub(super) fn terminal_cell(x: i32, y: i32, y_offset: u16) -> Option<(u16, u16)> {
    Some((
        u16::try_from(x).ok()?,
        u16::try_from(y).ok()?.saturating_add(y_offset),
    ))
}

impl RenderedPaneScrollbarRun {
    fn y_for(self, y: i32, y_offset: u16) -> Option<u16> {
        terminal_cell(i32::from(self.x), y, y_offset).map(|(_, y)| y)
    }
}

fn floating_frame_glyph(frame: SignedPaneRect, x: i32, y: i32) -> char {
    let right = frame.x().saturating_add(frame.cols()).saturating_sub(1);
    let bottom = frame.y().saturating_add(frame.rows()).saturating_sub(1);
    match (x == frame.x(), x == right, y == frame.y(), y == bottom) {
        (true, _, true, _) => '┌',
        (_, true, true, _) => '┐',
        (true, _, _, true) => '└',
        (_, true, _, true) => '┘',
        (true, _, _, _) | (_, true, _, _) => '│',
        _ => '─',
    }
}
