use rmux_core::style::{Style, StyleWidth};
use rmux_core::{OptionStore, PaneGeometry};
use rmux_proto::{OptionName, SessionName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollbarPosition {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneScrollbarsMode {
    Off,
    Modal,
    On,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneScrollbarConfig {
    pub(crate) mode: PaneScrollbarsMode,
    pub(crate) position: ScrollbarPosition,
    pub(crate) style: Style,
    pub(crate) width: u16,
    pub(crate) pad: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneScrollbarLayout {
    pub(crate) content: PaneGeometry,
    pub(crate) width: u16,
    pub(crate) pad: u16,
}

impl PaneScrollbarConfig {
    pub(crate) fn resolve(
        options: &OptionStore,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Self {
        let mode = match options.resolve_for_window(
            session_name,
            window_index,
            OptionName::PaneScrollbars,
        ) {
            Some("modal") => PaneScrollbarsMode::Modal,
            Some("on") => PaneScrollbarsMode::On,
            _ => PaneScrollbarsMode::Off,
        };
        let position = match options.resolve_for_window(
            session_name,
            window_index,
            OptionName::PaneScrollbarsPosition,
        ) {
            Some("left") => ScrollbarPosition::Left,
            _ => ScrollbarPosition::Right,
        };
        let style = options
            .resolve_for_pane(
                session_name,
                window_index,
                pane_index,
                OptionName::PaneScrollbarsStyle,
            )
            .and_then(|value| Style::parse(value).ok())
            .unwrap_or_default();
        let width = match style.width {
            Some(StyleWidth::Cells(width)) => u16::try_from(width.max(1)).unwrap_or(u16::MAX),
            Some(StyleWidth::Percentage(width)) => u16::from(width.max(1)),
            None => 1,
        };
        let pad = style
            .pad
            .and_then(|pad| u16::try_from(pad).ok())
            .unwrap_or(0);
        Self {
            mode,
            position,
            style,
            width,
            pad,
        }
    }

    pub(crate) const fn is_visible(&self, alternate_on: bool, copy_mode_active: bool) -> bool {
        if alternate_on {
            return false;
        }
        match self.mode {
            PaneScrollbarsMode::Off => false,
            PaneScrollbarsMode::Modal => copy_mode_active,
            PaneScrollbarsMode::On => true,
        }
    }

    pub(crate) fn content_geometry(
        &self,
        geometry: PaneGeometry,
        alternate_on: bool,
        copy_mode_active: bool,
    ) -> PaneGeometry {
        self.layout(geometry, alternate_on, copy_mode_active)
            .content
    }

    pub(crate) fn layout(
        &self,
        geometry: PaneGeometry,
        alternate_on: bool,
        copy_mode_active: bool,
    ) -> PaneScrollbarLayout {
        if !self.is_visible(alternate_on, copy_mode_active) {
            return PaneScrollbarLayout {
                content: geometry,
                width: 0,
                pad: 0,
            };
        }

        // Preserve PANE_MINIMUM (one content cell) and clip the configured
        // reservation to the pane. This also avoids tmux 3.7b's unsigned
        // underflow when oversized padding is combined with a left track.
        let available = geometry.cols().saturating_sub(1);
        let pad = self.pad.min(available);
        let width = self.width.min(available.saturating_sub(pad));
        let reserved = width.saturating_add(pad);
        let x = match self.position {
            ScrollbarPosition::Left => geometry.x().saturating_add(reserved),
            ScrollbarPosition::Right => geometry.x(),
        };
        PaneScrollbarLayout {
            content: PaneGeometry::new(
                x,
                geometry.y(),
                geometry.cols().saturating_sub(reserved),
                geometry.rows(),
            ),
            width,
            pad,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneScrollbar {
    pub(crate) position: ScrollbarPosition,
    pub(crate) width: u16,
    pub(crate) pad: u16,
    pub(crate) slider_y: u16,
    pub(crate) slider_h: u16,
}

impl PaneScrollbar {
    #[cfg(test)]
    pub(crate) fn from_config(
        rows: u16,
        history_size: usize,
        alternate_on: bool,
        config: &PaneScrollbarConfig,
        copy_mode_scroll_position: Option<usize>,
    ) -> Option<Self> {
        Self::from_dimensions(
            rows,
            history_size,
            alternate_on,
            config,
            config.width,
            config.pad,
            copy_mode_scroll_position,
        )
    }

    pub(crate) fn from_layout(
        layout: PaneScrollbarLayout,
        history_size: usize,
        alternate_on: bool,
        config: &PaneScrollbarConfig,
        copy_mode_scroll_position: Option<usize>,
    ) -> Option<Self> {
        Self::from_dimensions(
            layout.content.rows(),
            history_size,
            alternate_on,
            config,
            layout.width,
            layout.pad,
            copy_mode_scroll_position,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_dimensions(
        rows: u16,
        history_size: usize,
        alternate_on: bool,
        config: &PaneScrollbarConfig,
        width: u16,
        pad: u16,
        copy_mode_scroll_position: Option<usize>,
    ) -> Option<Self> {
        if rows == 0
            || (width == 0 && pad == 0)
            || !config.is_visible(alternate_on, copy_mode_scroll_position.is_some())
        {
            return None;
        }

        let scrollbar_height = usize::from(rows);
        let total_height = history_size.saturating_add(scrollbar_height).max(1);
        let scrollbar_height_float = f64::from(rows);
        let total_height_float = total_height as f64;
        let percent_view = scrollbar_height_float / total_height_float;
        let slider_height = (scrollbar_height_float * percent_view) as usize;
        let slider_height = slider_height.max(1).min(scrollbar_height);
        let mut slider_y = if let Some(scroll_position) = copy_mode_scroll_position {
            let current_offset = history_size.saturating_sub(scroll_position.min(history_size));
            ((scrollbar_height_float + 1.0) * (current_offset as f64 / total_height_float)) as usize
        } else {
            scrollbar_height.saturating_sub(slider_height)
        };
        if slider_y >= scrollbar_height {
            slider_y = scrollbar_height.saturating_sub(1);
        }

        Some(Self {
            position: config.position,
            width,
            pad,
            slider_y: u16::try_from(slider_y).unwrap_or(u16::MAX),
            slider_h: u16::try_from(slider_height).unwrap_or(u16::MAX),
        })
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_view(
        rows: u16,
        history_size: usize,
        alternate_on: bool,
        mode: PaneScrollbarsMode,
        position: ScrollbarPosition,
        width: u16,
        pad: u16,
        copy_mode_scroll_position: Option<usize>,
    ) -> Option<Self> {
        Self::from_config(
            rows,
            history_size,
            alternate_on,
            &PaneScrollbarConfig {
                mode,
                position,
                style: Style::default(),
                width,
                pad,
            },
            copy_mode_scroll_position,
        )
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::OptionStore;
    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, WindowTarget};

    use super::*;

    fn session_name() -> SessionName {
        SessionName::new("alpha").expect("valid session name")
    }

    fn resolved_config(style: &str) -> PaneScrollbarConfig {
        let session = session_name();
        let target = WindowTarget::with_window(session.clone(), 0);
        let mut options = OptionStore::new();
        for (option, value) in [
            (OptionName::PaneScrollbars, "on"),
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
        PaneScrollbarConfig::resolve(&options, &session, 0, 0)
    }

    #[test]
    fn left_scrollbar_reserves_width_and_padding_before_content() {
        let session = session_name();
        let mut options = OptionStore::new();
        let target = WindowTarget::with_window(session.clone(), 0);
        options
            .set(
                ScopeSelector::Window(target.clone()),
                OptionName::PaneScrollbars,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("scrollbar mode");
        options
            .set(
                ScopeSelector::Window(target.clone()),
                OptionName::PaneScrollbarsPosition,
                "left".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("scrollbar position");
        options
            .set(
                ScopeSelector::Window(target),
                OptionName::PaneScrollbarsStyle,
                "fg=red,bg=blue,width=2,pad=1".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("scrollbar style");
        let config = PaneScrollbarConfig::resolve(&options, &session, 0, 0);

        assert_eq!(config.width, 2);
        assert_eq!(config.pad, 1);
        assert_eq!(
            config.content_geometry(PaneGeometry::new(0, 0, 20, 8), false, false),
            PaneGeometry::new(3, 0, 17, 8)
        );
    }

    #[test]
    fn percentage_widths_use_the_numeric_tmux_3_7b_scrollbar_width() {
        // Measured against tmux 3.7b at a 20-column window: 0% and 1%
        // leave 19 content cells, 2% leaves 18, and 50% hits the
        // one-cell pane minimum.
        for (style, configured, content, track) in [
            ("width=0%", 1, 19, 1),
            ("width=1%", 1, 19, 1),
            ("width=2%", 2, 18, 2),
            ("width=50%", 50, 1, 19),
            ("width=21", 21, 1, 19),
        ] {
            let config = resolved_config(style);
            let layout = config.layout(PaneGeometry::new(0, 0, 20, 8), false, false);

            assert_eq!(config.width, configured, "{style}");
            assert_eq!(layout.content.cols(), content, "{style}");
            assert_eq!(layout.width, track, "{style}");
        }
    }

    #[test]
    fn one_column_pane_keeps_content_and_clips_the_scrollbar() {
        let config = resolved_config("width=2,pad=1");

        let layout = config.layout(PaneGeometry::new(7, 3, 1, 4), false, false);

        assert_eq!(layout.content, PaneGeometry::new(7, 3, 1, 4));
        assert_eq!((layout.width, layout.pad), (0, 0));
    }

    #[test]
    fn oversized_padding_is_clipped_before_the_track() {
        let config = resolved_config("width=2,pad=50");

        let layout = config.layout(PaneGeometry::new(0, 0, 20, 8), false, false);

        assert_eq!(layout.content, PaneGeometry::new(0, 0, 1, 8));
        assert_eq!((layout.width, layout.pad), (0, 19));
    }

    #[test]
    fn oversized_left_padding_preserves_one_content_cell() {
        let mut config = resolved_config("width=2,pad=50");
        config.position = ScrollbarPosition::Left;

        let layout = config.layout(PaneGeometry::new(0, 0, 20, 8), false, false);

        // Clipping must retain a usable content cell even when the configured
        // padding is wider than the pane.
        assert_eq!(layout.content, PaneGeometry::new(19, 0, 1, 8));
        assert_eq!((layout.width, layout.pad), (0, 19));
    }

    #[test]
    fn copy_mode_scroll_position_places_top_and_bottom_sliders_like_tmux() {
        let config = PaneScrollbarConfig {
            mode: PaneScrollbarsMode::On,
            position: ScrollbarPosition::Right,
            style: Style::default(),
            width: 1,
            pad: 0,
        };
        let at_top =
            PaneScrollbar::from_config(8, 23, false, &config, Some(23)).expect("top scrollbar");
        let at_bottom =
            PaneScrollbar::from_config(8, 23, false, &config, Some(0)).expect("bottom scrollbar");

        assert_eq!((at_top.slider_y, at_top.slider_h), (0, 2));
        assert_eq!((at_bottom.slider_y, at_bottom.slider_h), (6, 2));
    }

    #[test]
    fn copy_mode_slider_uses_tmux_3_7b_floating_point_truncation() {
        let config = PaneScrollbarConfig {
            mode: PaneScrollbarsMode::On,
            position: ScrollbarPosition::Right,
            style: Style::default(),
            width: 1,
            pad: 0,
        };

        // tmux 3.7b computes 22 * (45 / 66) in double precision and
        // truncates the resulting 14.999... to row 14.
        let scrollbar =
            PaneScrollbar::from_config(21, 45, false, &config, Some(0)).expect("scrollbar");

        assert_eq!((scrollbar.slider_y, scrollbar.slider_h), (14, 6));
    }

    #[test]
    fn alternate_screen_suppresses_even_always_on_scrollbar() {
        let config = PaneScrollbarConfig {
            mode: PaneScrollbarsMode::On,
            position: ScrollbarPosition::Right,
            style: Style::default(),
            width: 1,
            pad: 0,
        };

        assert!(PaneScrollbar::from_config(8, 23, true, &config, None).is_none());
        assert_eq!(
            config.content_geometry(PaneGeometry::new(0, 0, 20, 8), true, false),
            PaneGeometry::new(0, 0, 20, 8)
        );
    }
}
