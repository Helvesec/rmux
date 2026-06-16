use rmux_core::{OptionStore, PaneGeometry};
use rmux_proto::{OptionName, SessionName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneBorderStatusPosition {
    Top,
    Bottom,
}

impl PaneBorderStatusPosition {
    pub(crate) fn from_options(
        options: &OptionStore,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<Self> {
        match options.resolve_for_window(session_name, window_index, OptionName::PaneBorderStatus) {
            Some("top") => Some(Self::Top),
            Some("bottom") => Some(Self::Bottom),
            _ => None,
        }
    }
}

pub(crate) fn clip_pane_geometry(geometry: PaneGeometry, content_rows: u16) -> PaneGeometry {
    let y = geometry.y().min(content_rows);
    let rows = geometry.rows().min(content_rows.saturating_sub(y));
    PaneGeometry::new(geometry.x(), y, geometry.cols(), rows)
}

pub(crate) fn visible_pane_content_geometry(
    options: &OptionStore,
    session_name: &SessionName,
    window_index: u32,
    geometry: PaneGeometry,
    content_rows: u16,
) -> PaneGeometry {
    let geometry = clip_pane_geometry(geometry, content_rows);
    if geometry.rows() == 0 || content_rows == 0 {
        return geometry;
    }

    match PaneBorderStatusPosition::from_options(options, session_name, window_index) {
        Some(PaneBorderStatusPosition::Top) if geometry.y() == 0 => PaneGeometry::new(
            geometry.x(),
            geometry.y().saturating_add(1),
            geometry.cols(),
            geometry.rows().saturating_sub(1),
        ),
        Some(PaneBorderStatusPosition::Bottom)
            if geometry.y().saturating_add(geometry.rows()) >= content_rows =>
        {
            PaneGeometry::new(
                geometry.x(),
                geometry.y(),
                geometry.cols(),
                geometry.rows().saturating_sub(1),
            )
        }
        _ => geometry,
    }
}

pub(crate) fn pane_border_status_row(
    position: PaneBorderStatusPosition,
    geometry: PaneGeometry,
    content_rows: u16,
) -> Option<u16> {
    let geometry = clip_pane_geometry(geometry, content_rows);
    if geometry.rows() == 0 || content_rows == 0 {
        return None;
    }

    let row = match position {
        PaneBorderStatusPosition::Top => {
            if geometry.y() == 0 {
                0
            } else {
                geometry.y().saturating_sub(1)
            }
        }
        PaneBorderStatusPosition::Bottom => {
            let row = geometry.y().saturating_add(geometry.rows());
            if row >= content_rows {
                content_rows.saturating_sub(1)
            } else {
                row
            }
        }
    };
    (row < content_rows).then_some(row)
}

#[cfg(test)]
mod tests {
    use rmux_core::OptionStore;
    use rmux_proto::{ScopeSelector, SessionName, SetOptionMode, WindowTarget};

    use super::*;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn options_with_border_status(value: &str) -> (OptionStore, SessionName) {
        let session = session_name("alpha");
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
                OptionName::PaneBorderStatus,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .expect("pane-border-status set succeeds");
        (options, session)
    }

    #[test]
    fn top_status_reserves_only_the_top_edge_row() {
        let (options, session) = options_with_border_status("top");

        assert_eq!(
            visible_pane_content_geometry(
                &options,
                &session,
                0,
                PaneGeometry::new(0, 0, 80, 12),
                24,
            ),
            PaneGeometry::new(0, 1, 80, 11)
        );
        assert_eq!(
            visible_pane_content_geometry(
                &options,
                &session,
                0,
                PaneGeometry::new(0, 13, 80, 11),
                24,
            ),
            PaneGeometry::new(0, 13, 80, 11)
        );
        assert_eq!(
            pane_border_status_row(
                PaneBorderStatusPosition::Top,
                PaneGeometry::new(0, 0, 80, 12),
                24,
            ),
            Some(0)
        );
        assert_eq!(
            pane_border_status_row(
                PaneBorderStatusPosition::Top,
                PaneGeometry::new(0, 13, 80, 11),
                24,
            ),
            Some(12)
        );
    }

    #[test]
    fn bottom_status_reserves_only_the_bottom_edge_row() {
        let (options, session) = options_with_border_status("bottom");

        assert_eq!(
            visible_pane_content_geometry(
                &options,
                &session,
                0,
                PaneGeometry::new(0, 0, 80, 12),
                24,
            ),
            PaneGeometry::new(0, 0, 80, 12)
        );
        assert_eq!(
            visible_pane_content_geometry(
                &options,
                &session,
                0,
                PaneGeometry::new(0, 13, 80, 11),
                24,
            ),
            PaneGeometry::new(0, 13, 80, 10)
        );
        assert_eq!(
            pane_border_status_row(
                PaneBorderStatusPosition::Bottom,
                PaneGeometry::new(0, 0, 80, 12),
                24,
            ),
            Some(12)
        );
        assert_eq!(
            pane_border_status_row(
                PaneBorderStatusPosition::Bottom,
                PaneGeometry::new(0, 13, 80, 11),
                24,
            ),
            Some(23)
        );
    }
}
