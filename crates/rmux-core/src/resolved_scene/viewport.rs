use super::{ResolvedScene, SignedPaneRect, TerminalSize};

impl ResolvedScene {
    /// Translates an already resolved canonical scene into a client viewport.
    /// Source rectangles stay signed; clipping never rewrites pane-local origins.
    #[must_use]
    pub fn into_viewport(mut self, x: u16, y: u16, size: TerminalSize) -> Self {
        let translate = |rect: SignedPaneRect| SignedPaneRect {
            x: rect.x.saturating_sub(i32::from(x)),
            y: rect.y.saturating_sub(i32::from(y)),
            ..rect
        };
        for pane in &mut self.panes {
            pane.content = translate(pane.content);
            pane.frame = translate(pane.frame);
            pane.content_clip = translate(pane.content_clip).clip_to(size);
            pane.frame_clip = translate(pane.frame_clip).clip_to(size);
            for region in &mut pane.hit_regions {
                region.rect = translate(region.rect);
                region.clip = translate(region.clip).clip_to(size);
            }
        }
        self.context.content_size = size;
        self
    }
}
