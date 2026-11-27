use rmux_core::{PaneGeometry, ResolvedHitRegionKind, ResolvedPaneScene, SignedPaneRect};

use crate::pane_scrollbar::{PaneScrollbarConfig, PaneScrollbarLayout, ScrollbarPosition};

/// Application source coordinates and their visible screen intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MouseCoordinates {
    pub(super) source: SignedPaneRect,
    pub(super) clip: SignedPaneRect,
    viewport_offset: (u16, u16),
}

impl From<PaneGeometry> for MouseCoordinates {
    fn from(pane: PaneGeometry) -> Self {
        let source = SignedPaneRect::new(
            pane.x().into(),
            pane.y().into(),
            pane.cols().into(),
            pane.rows().into(),
        )
        .unwrap();
        Self {
            source,
            clip: source,
            viewport_offset: (0, 0),
        }
    }
}

impl MouseCoordinates {
    pub(super) fn with_viewport_offset(mut self, offset: (u16, u16)) -> Self {
        self.viewport_offset = offset;
        self
    }

    /// Converts a point below the status line to canonical window coordinates.
    pub(crate) fn window_position(self, x: u16, y: u16) -> (i32, i32) {
        (
            i32::from(x) + i32::from(self.viewport_offset.0),
            i32::from(y) + i32::from(self.viewport_offset.1),
        )
    }

    pub(crate) fn relative_position(self, x: u16, y: u16) -> Option<(u16, u16)> {
        contains(self.clip, x, y).then(|| self.clamped_offset(x, y))
    }

    pub(super) fn clamped_offset(self, x: u16, y: u16) -> (u16, u16) {
        let offset = |value: u16, origin: i32| {
            u16::try_from(i32::from(value).saturating_sub(origin).max(0)).unwrap_or(u16::MAX)
        };
        (offset(x, self.source.x()), offset(y, self.source.y()))
    }

    pub(super) fn cols(self) -> u16 {
        u16::try_from(self.source.cols()).unwrap_or(u16::MAX)
    }
    pub(super) fn rows(self) -> u16 {
        u16::try_from(self.source.rows()).unwrap_or(u16::MAX)
    }
    pub(super) fn above(self, y: u16) -> bool {
        i32::from(y) < self.source.y()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MouseProjection {
    pub(super) scene: ResolvedPaneScene,
    pub(super) coordinates: MouseCoordinates,
    pub(super) track: SignedPaneRect,
}

impl MouseProjection {
    pub(super) fn new(
        scene: &ResolvedPaneScene,
        config: &PaneScrollbarConfig,
        alternate: bool,
        copy_mode: bool,
    ) -> (Self, PaneScrollbarLayout) {
        // The producer's source content regions already exclude pane status.
        // Retain their signed extent; never reserve a track on the viewport clip.
        let source = scene
            .hit_regions()
            .iter()
            .filter(|region| region.kind() == ResolvedHitRegionKind::Content)
            .map(|region| region.rect())
            .reduce(|a, b| {
                let x = a.x().min(b.x());
                let y = a.y().min(b.y());
                SignedPaneRect::new(
                    x,
                    y,
                    (a.x().saturating_add(a.cols()))
                        .max(b.x().saturating_add(b.cols()))
                        .saturating_sub(x),
                    (a.y().saturating_add(a.rows()))
                        .max(b.y().saturating_add(b.rows()))
                        .saturating_sub(y),
                )
                .unwrap()
            })
            .unwrap_or(
                SignedPaneRect::new(scene.content().x(), scene.content().y(), 0, 0).unwrap(),
            );
        let local = config.layout(
            PaneGeometry::new(
                0,
                0,
                u16::try_from(source.cols()).unwrap_or(u16::MAX),
                u16::try_from(source.rows()).unwrap_or(u16::MAX),
            ),
            alternate,
            copy_mode,
        );
        let content = SignedPaneRect::new(
            source.x().saturating_add(local.content.x().into()),
            source.y(),
            local.content.cols().into(),
            local.content.rows().into(),
        )
        .unwrap();
        let track_x = match config.position {
            ScrollbarPosition::Left => source.x(),
            ScrollbarPosition::Right => content
                .x()
                .saturating_add(content.cols())
                .saturating_add(local.pad.into()),
        };
        let track =
            SignedPaneRect::new(track_x, content.y(), local.width.into(), content.rows()).unwrap();
        (
            Self {
                scene: scene.clone(),
                coordinates: MouseCoordinates {
                    source: content,
                    clip: intersection(content, scene.content_clip()),
                    viewport_offset: (0, 0),
                },
                track,
            },
            local,
        )
    }

    pub(super) fn region_at(&self, x: u16, y: u16) -> Option<ResolvedHitRegionKind> {
        self.scene
            .hit_regions()
            .iter()
            .find(|region| contains(region.clip(), x, y))
            .map(|region| region.kind())
    }
}

pub(super) fn contains(rect: SignedPaneRect, x: u16, y: u16) -> bool {
    let (x, y) = (i32::from(x), i32::from(y));
    x >= rect.x()
        && y >= rect.y()
        && x < rect.x().saturating_add(rect.cols())
        && y < rect.y().saturating_add(rect.rows())
}

fn intersection(a: SignedPaneRect, b: SignedPaneRect) -> SignedPaneRect {
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
    .unwrap()
}
