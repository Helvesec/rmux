use rmux_proto::TerminalSize;

use crate::{FloatingPaneRect, Pane, PaneGeometry, PaneId, Window, WindowId};

/// A signed rectangle in window content coordinates.
///
/// Coordinates are signed so floating panes can retain off-screen placement
/// without losing negative offsets. Width and height are non-negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedPaneRect {
    x: i32,
    y: i32,
    cols: i32,
    rows: i32,
}

impl SignedPaneRect {
    /// Creates a signed rectangle when its dimensions are non-negative.
    #[must_use]
    pub fn new(x: i32, y: i32, cols: i32, rows: i32) -> Option<Self> {
        (cols >= 0 && rows >= 0).then_some(Self { x, y, cols, rows })
    }

    /// Returns the signed left column.
    #[must_use]
    pub const fn x(self) -> i32 {
        self.x
    }

    /// Returns the signed top row.
    #[must_use]
    pub const fn y(self) -> i32 {
        self.y
    }

    /// Returns the width in cells.
    #[must_use]
    pub const fn cols(self) -> i32 {
        self.cols
    }

    /// Returns the height in cells.
    #[must_use]
    pub const fn rows(self) -> i32 {
        self.rows
    }

    /// Returns whether either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.cols == 0 || self.rows == 0
    }

    /// Clips this rectangle to an unsigned terminal-sized viewport.
    #[must_use]
    pub fn clip_to(self, bounds: TerminalSize) -> Self {
        let max_x = i32::from(bounds.cols);
        let max_y = i32::from(bounds.rows);
        let left = self.x.clamp(0, max_x);
        let top = self.y.clamp(0, max_y);
        let right = self.x.saturating_add(self.cols).clamp(0, max_x);
        let bottom = self.y.saturating_add(self.rows).clamp(0, max_y);
        Self {
            x: left.min(right),
            y: top.min(bottom),
            cols: right.saturating_sub(left).max(0),
            rows: bottom.saturating_sub(top).max(0),
        }
    }

    /// Converts a clipped non-negative rectangle into pane geometry.
    #[must_use]
    pub fn pane_geometry(self) -> Option<PaneGeometry> {
        Some(PaneGeometry::new(
            u16::try_from(self.x).ok()?,
            u16::try_from(self.y).ok()?,
            u16::try_from(self.cols).ok()?,
            u16::try_from(self.rows).ok()?,
        ))
    }

    fn from_pane_geometry(geometry: PaneGeometry) -> Self {
        Self {
            x: i32::from(geometry.x()),
            y: i32::from(geometry.y()),
            cols: i32::from(geometry.cols()),
            rows: i32::from(geometry.rows()),
        }
    }

    fn from_floating_rect(rect: FloatingPaneRect) -> Self {
        Self {
            x: rect.x(),
            y: rect.y(),
            cols: rect.cols(),
            rows: rect.rows(),
        }
    }

    fn frame(self) -> Self {
        Self {
            x: self.x.saturating_sub(1),
            y: self.y.saturating_sub(1),
            cols: self.cols.saturating_add(2),
            rows: self.rows.saturating_add(2),
        }
    }
}

/// Pane-border status placement that affects visible pane content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedPaneBorderStatus {
    /// No pane-border status line is reserved.
    Off,
    /// A pane-border status line is reserved above edge panes.
    Top,
    /// A pane-border status line is reserved below edge panes.
    Bottom,
}

/// How zoomed windows contribute panes to a resolved scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedZoomVisibility {
    /// Include only panes that are render-visible.
    Rendered,
    /// Retain hidden zoom siblings for layout-style consumers.
    IncludeHidden,
}

/// Context supplied by the caller when resolving a scene from a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSceneContext {
    content_size: TerminalSize,
    pane_border_status: ResolvedPaneBorderStatus,
    zoom_visibility: ResolvedZoomVisibility,
}

impl ResolvedSceneContext {
    /// Creates a resolution context for a window content viewport.
    #[must_use]
    pub const fn new(
        content_size: TerminalSize,
        pane_border_status: ResolvedPaneBorderStatus,
        zoom_visibility: ResolvedZoomVisibility,
    ) -> Self {
        Self {
            content_size,
            pane_border_status,
            zoom_visibility,
        }
    }

    /// Returns the content viewport used for clipping.
    #[must_use]
    pub const fn content_size(self) -> TerminalSize {
        self.content_size
    }

    /// Returns the resolved pane-border status placement.
    #[must_use]
    pub const fn pane_border_status(self) -> ResolvedPaneBorderStatus {
        self.pane_border_status
    }

    /// Returns the requested zoom visibility policy.
    #[must_use]
    pub const fn zoom_visibility(self) -> ResolvedZoomVisibility {
        self.zoom_visibility
    }
}

/// A resolved pane layer in rear-to-front order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedPaneLayer {
    /// Tiled panes share the base layer.
    Tiled,
    /// Floating panes use their rear-to-front stack position.
    Floating(u32),
}

impl ResolvedPaneLayer {
    /// Returns a numeric ordering key where larger values are closer to front.
    #[must_use]
    pub const fn z_index(self) -> u32 {
        match self {
            Self::Tiled => 0,
            Self::Floating(index) => index,
        }
    }
}

/// A hit-testable region projected from resolved pane geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedHitRegionKind {
    /// Pane application content.
    Content,
    /// Pane frame or border cells.
    Frame,
    /// Pane-border status row.
    PaneBorderStatus,
}

/// One signed hit region and its clipped visible portion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedHitRegion {
    kind: ResolvedHitRegionKind,
    rect: SignedPaneRect,
    clip: SignedPaneRect,
}

impl ResolvedHitRegion {
    /// Returns the kind of hit region.
    #[must_use]
    pub const fn kind(self) -> ResolvedHitRegionKind {
        self.kind
    }

    /// Returns the signed source rectangle.
    #[must_use]
    pub const fn rect(self) -> SignedPaneRect {
        self.rect
    }

    /// Returns the clipped visible portion of the source rectangle.
    #[must_use]
    pub const fn clip(self) -> SignedPaneRect {
        self.clip
    }
}

/// One pane projected into an immutable resolved scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPaneScene {
    pane_id: PaneId,
    pane_index: u32,
    layer: ResolvedPaneLayer,
    focused: bool,
    visible: bool,
    content: SignedPaneRect,
    frame: SignedPaneRect,
    content_clip: SignedPaneRect,
    frame_clip: SignedPaneRect,
    hit_regions: Vec<ResolvedHitRegion>,
}

impl ResolvedPaneScene {
    /// Returns the stable pane identity.
    #[must_use]
    pub const fn pane_id(&self) -> PaneId {
        self.pane_id
    }

    /// Returns the window-local pane index.
    #[must_use]
    pub const fn pane_index(&self) -> u32 {
        self.pane_index
    }

    /// Returns this pane's z layer.
    #[must_use]
    pub const fn layer(&self) -> ResolvedPaneLayer {
        self.layer
    }

    /// Returns whether this pane is the window focus target.
    #[must_use]
    pub const fn focused(&self) -> bool {
        self.focused
    }

    /// Returns whether this pane should be visible in this scene context.
    #[must_use]
    pub const fn visible(&self) -> bool {
        self.visible
    }

    /// Returns the signed pane content rectangle before clipping.
    #[must_use]
    pub const fn content(&self) -> SignedPaneRect {
        self.content
    }

    /// Returns the signed pane frame rectangle before clipping.
    #[must_use]
    pub const fn frame(&self) -> SignedPaneRect {
        self.frame
    }

    /// Returns the clipped pane content rectangle.
    #[must_use]
    pub const fn content_clip(&self) -> SignedPaneRect {
        self.content_clip
    }

    /// Returns the clipped pane frame rectangle.
    #[must_use]
    pub const fn frame_clip(&self) -> SignedPaneRect {
        self.frame_clip
    }

    /// Returns the clipped content as unsigned pane geometry.
    #[must_use]
    pub fn content_clip_geometry(&self) -> PaneGeometry {
        self.content_clip
            .pane_geometry()
            .expect("scene clips are bounded by a TerminalSize")
    }

    /// Returns the hit regions derived for this pane.
    #[must_use]
    pub fn hit_regions(&self) -> &[ResolvedHitRegion] {
        &self.hit_regions
    }
}

/// Immutable geometry projection for a canonical window and caller context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScene {
    window_id: WindowId,
    context: ResolvedSceneContext,
    active_pane_id: Option<PaneId>,
    panes: Vec<ResolvedPaneScene>,
}

impl ResolvedScene {
    /// Returns the stable window identity used to derive this scene.
    #[must_use]
    pub const fn window_id(&self) -> WindowId {
        self.window_id
    }

    /// Returns the resolution context used to derive this scene.
    #[must_use]
    pub const fn context(&self) -> ResolvedSceneContext {
        self.context
    }

    /// Returns the active pane identity in the source window.
    #[must_use]
    pub const fn active_pane_id(&self) -> Option<PaneId> {
        self.active_pane_id
    }

    /// Returns panes in stable rear-to-front scene order.
    #[must_use]
    pub fn panes(&self) -> &[ResolvedPaneScene] {
        &self.panes
    }

    /// Returns the scene pane for a stable pane identity.
    #[must_use]
    pub fn pane_by_id(&self, pane_id: PaneId) -> Option<&ResolvedPaneScene> {
        self.panes.iter().find(|pane| pane.pane_id == pane_id)
    }

    /// Returns the scene pane for a window-local pane index.
    #[must_use]
    pub fn pane_by_index(&self, pane_index: u32) -> Option<&ResolvedPaneScene> {
        self.panes.iter().find(|pane| pane.pane_index == pane_index)
    }
}

impl Window {
    /// Resolves this canonical window into an immutable geometric scene.
    #[must_use]
    pub fn resolved_scene(&self, context: ResolvedSceneContext) -> ResolvedScene {
        let active_pane_id = self.active_pane().map(Pane::id);
        let floating_stack = self.floating_stack();
        let layout_tree = self.current_layout_tree();
        let mut panes = Vec::with_capacity(self.panes().len());

        for pane in self.panes() {
            if floating_stack.placement(pane.id()).is_none() {
                panes.push(resolve_pane_scene(
                    self,
                    context,
                    pane,
                    active_pane_id,
                    ResolvedPaneLayer::Tiled,
                    (
                        SignedPaneRect::from_pane_geometry(
                            layout_tree
                                .as_ref()
                                .and_then(|tree| tree.pane_geometry(pane.id()))
                                .expect("tiled scene panes must own a canonical layout cell"),
                        ),
                        true,
                    ),
                ));
            }
        }

        for (index, placement) in floating_stack.placements().iter().enumerate() {
            let Some(pane) = self
                .panes()
                .iter()
                .find(|pane| pane.id() == placement.pane_id())
            else {
                continue;
            };
            panes.push(resolve_pane_scene(
                self,
                context,
                pane,
                active_pane_id,
                ResolvedPaneLayer::Floating(u32::try_from(index + 1).unwrap_or(u32::MAX)),
                (
                    SignedPaneRect::from_floating_rect(placement.rect()),
                    placement.visible(),
                ),
            ));
        }

        ResolvedScene {
            window_id: self.id(),
            context,
            active_pane_id,
            panes,
        }
    }
}

fn resolve_pane_scene(
    window: &Window,
    context: ResolvedSceneContext,
    pane: &Pane,
    active_pane_id: Option<PaneId>,
    layer: ResolvedPaneLayer,
    placement: (SignedPaneRect, bool),
) -> ResolvedPaneScene {
    let focused = active_pane_id == Some(pane.id());
    let zoom_hidden = window.is_zoomed()
        && !focused
        && context.zoom_visibility == ResolvedZoomVisibility::Rendered;
    let visible = placement.1 && !zoom_hidden;
    let content = if window.is_zoomed() && focused {
        SignedPaneRect::from_pane_geometry(PaneGeometry::new(
            0,
            0,
            context.content_size.cols,
            context.content_size.rows,
        ))
    } else {
        placement.0
    };
    let raw_content_clip = content.clip_to(context.content_size);
    let content_clip = content_clip_for_border_status(raw_content_clip, context);
    let frame = content.frame();
    let frame_clip = frame.clip_to(context.content_size);
    let hit_regions = regions::hit_regions_for_pane(content, raw_content_clip, frame, context);

    ResolvedPaneScene {
        pane_id: pane.id(),
        pane_index: pane.index(),
        layer,
        focused,
        visible,
        content,
        frame,
        content_clip,
        frame_clip,
        hit_regions,
    }
}

fn content_clip_for_border_status(
    geometry: SignedPaneRect,
    context: ResolvedSceneContext,
) -> SignedPaneRect {
    if geometry.is_empty() {
        return geometry;
    }

    match context.pane_border_status {
        ResolvedPaneBorderStatus::Top if geometry.y() == 0 => SignedPaneRect {
            y: geometry.y().saturating_add(1),
            rows: geometry.rows().saturating_sub(1),
            ..geometry
        },
        ResolvedPaneBorderStatus::Bottom
            if geometry.y().saturating_add(geometry.rows())
                >= i32::from(context.content_size.rows) =>
        {
            SignedPaneRect {
                rows: geometry.rows().saturating_sub(1),
                ..geometry
            }
        }
        ResolvedPaneBorderStatus::Off | ResolvedPaneBorderStatus::Top => geometry,
        ResolvedPaneBorderStatus::Bottom => geometry,
    }
}

fn pane_border_status_rect(
    geometry: SignedPaneRect,
    context: ResolvedSceneContext,
) -> Option<SignedPaneRect> {
    if geometry.is_empty() || context.content_size.rows == 0 {
        return None;
    }
    let y = match context.pane_border_status {
        ResolvedPaneBorderStatus::Off => return None,
        ResolvedPaneBorderStatus::Top => {
            if geometry.y() == 0 {
                0
            } else {
                geometry.y().saturating_sub(1)
            }
        }
        ResolvedPaneBorderStatus::Bottom => {
            let bottom = geometry.y().saturating_add(geometry.rows());
            bottom.min(i32::from(context.content_size.rows).saturating_sub(1))
        }
    };
    Some(SignedPaneRect {
        x: geometry.x(),
        y,
        cols: geometry.cols(),
        rows: 1,
    })
}

mod regions;

#[cfg(test)]
#[path = "resolved_scene/tests.rs"]
mod tests;

mod viewport;
