use super::*;
use rmux_proto::scene::{
    SceneBorder, SceneHitRegion, ScenePane, SceneRect, SceneSnapshot, SceneSpan,
};

fn rect(r: SignedPaneRect) -> SceneRect {
    SceneRect {
        x: r.x(),
        y: r.y(),
        cols: r.cols(),
        rows: r.rows(),
    }
}

impl TerminalSceneProjection {
    // Raw application-grid projection. Scrollbar/status decoration remains owned
    // by the terminal renderer; consumers never recalculate layout or occlusion.
    pub(in crate::renderer) fn snapshot(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
    ) -> Result<SceneSnapshot, rmux_proto::RmuxError> {
        let panes = self
            .scene
            .panes()
            .iter()
            .map(|p| {
                let spans = session
                    .window()
                    .panes()
                    .iter()
                    .find(|v| v.id() == p.pane_id())
                    .and_then(|pane| self.raw_pane_projection(pane))
                    .map(|projection| {
                        projection
                            .spans
                            .iter()
                            .map(|s| SceneSpan {
                                x: s.x,
                                y: s.y,
                                source_row: s.source_row,
                                source_col: s.source_col,
                                cols: s.cols,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ScenePane {
                    id: p.pane_id(),
                    index: p.pane_index(),
                    floating: p.layer() != ResolvedPaneLayer::Tiled,
                    layer: p.layer().z_index(),
                    focused: p.focused(),
                    visible: p.visible(),
                    content: rect(p.content()),
                    frame: rect(p.frame()),
                    content_clip: rect(p.content_clip()),
                    frame_clip: rect(p.frame_clip()),
                    hit_regions: p
                        .hit_regions()
                        .iter()
                        .map(|h| SceneHitRegion {
                            kind: match h.kind() {
                                ResolvedHitRegionKind::Content => "content",
                                ResolvedHitRegionKind::Frame => "frame",
                                ResolvedHitRegionKind::PaneBorderStatus => "pane-border-status",
                            }
                            .to_owned(),
                            rect: rect(h.rect()),
                            clip: rect(h.clip()),
                        })
                        .collect(),
                    spans,
                }
            })
            .collect();
        let borders = self
            .floating_border_cells()
            .into_iter()
            .filter_map(|b| {
                self.scene.pane_by_index(b.pane_index).map(|p| SceneBorder {
                    x: b.x,
                    y: b.y,
                    glyph: b.glyph,
                    pane_id: p.pane_id(),
                    active: b.active,
                })
            })
            .collect();
        let mut snapshot = SceneSnapshot {
            schema: 1,
            session_id: session.id(),
            window_id: self.scene.window_id(),
            window_index: session.active_window_index(),
            revision: String::new(),
            cols: self.geometry.terminal_size.cols,
            rows: self.geometry.terminal_size.rows,
            content_y_offset: self.geometry.content_y_offset,
            active_pane_id: self.scene.active_pane_id(),
            panes,
            borders,
        };
        snapshot.seal()?;
        Ok(snapshot)
    }
}

pub(crate) fn window_scene(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    query: &rmux_proto::scene::SceneQuery,
) -> Result<SceneSnapshot, rmux_proto::RmuxError> {
    let window = session
        .window_at(query.window_index)
        .ok_or_else(|| rmux_proto::RmuxError::Server("scene window disappeared".to_owned()))?;
    if query
        .expected_identity
        .is_some_and(|ids| ids != (session.id(), window.id()))
    {
        return Err(rmux_proto::RmuxError::Server(
            "scene selection changed identity".to_owned(),
        ));
    }
    let mut selected = rmux_core::ManagedSessionPresentation::new(session);
    selected.select_window(query.window_index)?;
    let view = selected.view();
    TerminalSceneProjection::resolve(view, options).snapshot(view)
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;
