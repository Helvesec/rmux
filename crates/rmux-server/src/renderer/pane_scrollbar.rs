use rmux_core::style::Style;
use rmux_core::{OptionStore, Pane, PaneGeometry, Session};

use super::{cursor_position_bytes, style_sgr_bytes};
use crate::pane_scrollbar::{PaneScrollbar, PaneScrollbarConfig, ScrollbarPosition};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderedPaneScrollbar {
    track_x: u16,
    pad_x: u16,
    y: u16,
    rows: u16,
    scrollbar: PaneScrollbar,
    track_style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaneScrollbarRenderContext {
    pub(super) geometry: PaneGeometry,
    pub(super) history_size: usize,
    pub(super) alternate_on: bool,
    pub(super) copy_mode_scroll_position: Option<usize>,
    pub(super) content_y_offset: u16,
}

pub(super) fn resolve_pane_scrollbar(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    context: PaneScrollbarRenderContext,
) -> (PaneGeometry, Option<RenderedPaneScrollbar>) {
    let PaneScrollbarRenderContext {
        geometry,
        history_size,
        alternate_on,
        copy_mode_scroll_position,
        content_y_offset,
    } = context;
    let config = PaneScrollbarConfig::resolve(
        options,
        session.name(),
        session.active_window_index(),
        pane.index(),
    );
    let layout = config.layout(geometry, alternate_on, copy_mode_scroll_position.is_some());
    let content = layout.content;
    let scrollbar = PaneScrollbar::from_layout(
        layout,
        history_size,
        alternate_on,
        &config,
        copy_mode_scroll_position,
    );
    let rendered = scrollbar.map(|scrollbar| {
        let (track_x, pad_x) = match scrollbar.position {
            ScrollbarPosition::Left => {
                let track_x = content
                    .x()
                    .saturating_sub(scrollbar.width.saturating_add(scrollbar.pad));
                (track_x, track_x.saturating_add(scrollbar.width))
            }
            ScrollbarPosition::Right => {
                let pad_x = content.x().saturating_add(content.cols());
                (pad_x.saturating_add(scrollbar.pad), pad_x)
            }
        };
        RenderedPaneScrollbar {
            track_x,
            pad_x,
            y: content.y().saturating_add(content_y_offset),
            rows: content.rows(),
            scrollbar,
            track_style: config.style,
        }
    });
    (content, rendered)
}

impl RenderedPaneScrollbar {
    pub(super) fn frame(&self) -> Vec<u8> {
        let mut frame = Vec::with_capacity(
            usize::from(self.rows)
                .saturating_mul(usize::from(self.scrollbar.width) + 40)
                .saturating_add(16),
        );
        let mut slider_style = self.track_style.clone();
        std::mem::swap(&mut slider_style.cell.fg, &mut slider_style.cell.bg);
        let track = " ".repeat(usize::from(self.scrollbar.width));
        let pad = " ".repeat(usize::from(self.scrollbar.pad));

        for row in 0..self.rows {
            if self.scrollbar.pad != 0 {
                frame.extend_from_slice(
                    cursor_position_bytes(self.y.saturating_add(row), self.pad_x).as_slice(),
                );
                frame.extend_from_slice(b"\x1b[0m");
                frame.extend_from_slice(pad.as_bytes());
            }
            if self.scrollbar.width != 0 {
                frame.extend_from_slice(
                    cursor_position_bytes(self.y.saturating_add(row), self.track_x).as_slice(),
                );
                let in_slider = row >= self.scrollbar.slider_y
                    && row
                        < self
                            .scrollbar
                            .slider_y
                            .saturating_add(self.scrollbar.slider_h);
                let style = if in_slider {
                    &slider_style
                } else {
                    &self.track_style
                };
                frame.extend_from_slice(scrollbar_style_sgr_bytes(style).as_slice());
                frame.extend_from_slice(track.as_bytes());
            }
        }
        frame.extend_from_slice(b"\x1b[0m");
        frame
    }
}

fn scrollbar_style_sgr_bytes(style: &Style) -> Vec<u8> {
    let sgr = style_sgr_bytes(style, false);
    if sgr == b"\x1b[0m" || sgr.starts_with(b"\x1b[0;") {
        return sgr;
    }

    // Each row is addressed independently. Prefix the parameter list with a
    // reset so a default background cannot inherit the preceding slider's
    // inverted background.
    let mut reset = Vec::with_capacity(sgr.len().saturating_add(2));
    reset.extend_from_slice(b"\x1b[0;");
    reset.extend_from_slice(sgr.strip_prefix(b"\x1b[").unwrap_or(sgr.as_slice()));
    reset
}

#[cfg(test)]
mod tests {
    use rmux_core::{OptionStore, PaneGeometry, Session};
    use rmux_proto::{
        OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
    };

    use super::*;

    fn fixture(mode: &str, position: &str, style: &str) -> (Session, OptionStore) {
        let session_name = SessionName::new("alpha").expect("valid session name");
        let session = Session::new(session_name.clone(), TerminalSize { cols: 20, rows: 8 });
        let target = WindowTarget::with_window(session_name, 0);
        let mut options = OptionStore::new();
        for (option, value) in [
            (OptionName::PaneScrollbars, mode),
            (OptionName::PaneScrollbarsPosition, position),
            (OptionName::PaneScrollbarsStyle, style),
        ] {
            options
                .set(
                    ScopeSelector::Window(target.clone()),
                    option,
                    value.to_owned(),
                    SetOptionMode::Replace,
                )
                .expect("scrollbar option");
        }
        (session, options)
    }

    #[test]
    fn right_scrollbar_frame_paints_track_and_inverted_slider() {
        let (session, options) = fixture("on", "right", "bg=black,fg=white,width=1,pad=0");
        let pane = session.window().pane(0).expect("pane exists");
        let (content, scrollbar) = resolve_pane_scrollbar(
            &session,
            &options,
            pane,
            PaneScrollbarRenderContext {
                geometry: PaneGeometry::new(0, 0, 20, 8),
                history_size: 23,
                alternate_on: false,
                copy_mode_scroll_position: None,
                content_y_offset: 0,
            },
        );
        let frame = scrollbar.expect("scrollbar").frame();

        assert_eq!(content, PaneGeometry::new(0, 0, 19, 8));
        assert!(frame
            .windows(b"\x1b[1;20H\x1b[0;37;40m ".len())
            .any(|window| window == b"\x1b[1;20H\x1b[0;37;40m "));
        assert!(frame
            .windows(b"\x1b[7;20H\x1b[0;30;47m ".len())
            .any(|window| window == b"\x1b[7;20H\x1b[0;30;47m "));
    }

    #[test]
    fn left_scrollbar_frame_places_track_before_padding() {
        let (session, options) = fixture("on", "left", "fg=red,bg=blue,width=2,pad=1");
        let pane = session.window().pane(0).expect("pane exists");
        let (content, scrollbar) = resolve_pane_scrollbar(
            &session,
            &options,
            pane,
            PaneScrollbarRenderContext {
                geometry: PaneGeometry::new(0, 0, 20, 8),
                history_size: 23,
                alternate_on: false,
                copy_mode_scroll_position: None,
                content_y_offset: 0,
            },
        );
        let frame = scrollbar.expect("scrollbar").frame();

        assert_eq!(content, PaneGeometry::new(3, 0, 17, 8));
        assert!(frame
            .windows(b"\x1b[1;3H\x1b[0m \x1b[1;1H\x1b[0;31;44m  ".len())
            .any(|window| window == b"\x1b[1;3H\x1b[0m \x1b[1;1H\x1b[0;31;44m  "));
    }

    #[test]
    fn scrollbar_track_ignores_fill_like_tmux_3_7b() {
        let (session, options) =
            fixture("on", "right", "fill=red,fg=white,bg=default,width=1,pad=0");
        let pane = session.window().pane(0).expect("pane exists");
        let (_, scrollbar) = resolve_pane_scrollbar(
            &session,
            &options,
            pane,
            PaneScrollbarRenderContext {
                geometry: PaneGeometry::new(0, 0, 20, 8),
                history_size: 23,
                alternate_on: false,
                copy_mode_scroll_position: None,
                content_y_offset: 0,
            },
        );

        let frame = scrollbar.expect("scrollbar").frame();

        assert!(frame
            .windows(b"\x1b[0;37m".len())
            .any(|run| run == b"\x1b[0;37m"));
        assert!(
            !frame
                .windows(b"\x1b[41m".len())
                .any(|run| run == b"\x1b[41m"),
            "tmux 3.7b ignores the style fill colour for scrollbar cells"
        );
    }

    #[test]
    fn modal_and_alternate_visibility_match_tmux_oracle() {
        let (session, options) = fixture("modal", "right", "bg=black,fg=white,width=1,pad=0");
        let pane = session.window().pane(0).expect("pane exists");
        let raw = PaneGeometry::new(0, 0, 20, 8);

        let context = |alternate_on, copy_mode_scroll_position| PaneScrollbarRenderContext {
            geometry: raw,
            history_size: 23,
            alternate_on,
            copy_mode_scroll_position,
            content_y_offset: 0,
        };
        let (normal, normal_scrollbar) =
            resolve_pane_scrollbar(&session, &options, pane, context(false, None));
        let (copy, copy_scrollbar) =
            resolve_pane_scrollbar(&session, &options, pane, context(false, Some(23)));
        let (alternate, alternate_scrollbar) =
            resolve_pane_scrollbar(&session, &options, pane, context(true, Some(23)));

        assert_eq!(normal, raw);
        assert!(normal_scrollbar.is_none());
        assert_eq!(copy, PaneGeometry::new(0, 0, 19, 8));
        assert!(copy_scrollbar.is_some());
        assert_eq!(alternate, raw);
        assert!(alternate_scrollbar.is_none());
    }
}
