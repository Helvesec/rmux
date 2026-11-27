//! Frozen downstream root import; no source-derived expectations.
fn main() {
    let _: fn(&rmux_sdk::PaneCell) -> ratatui_core::style::Style = ratatui_rmux::cell_style;
    let _: fn(rmux_sdk::PaneColor) -> ratatui_core::style::Color = ratatui_rmux::color;
    let _ = ratatui_rmux::PaneWidget::new;
}
