# ratatui-rmux

Ratatui integration for the [RMUX](https://github.com/Helvesec/rmux) terminal multiplexer.

Paints daemon-backed pane snapshots into a `ratatui::buffer::Buffer` with no
async work in the draw path. The async driver pulls snapshots from the
RMUX daemon; the widget is a deterministic, sync projection of the captured
state.

## Usage

For a 0.11.0 daemon (wire 9), use the adapter and SDK from the same checkout.
Replace `/path/to/rmux` in both paths with its location. Once both 0.11.0
crates are published, you can omit the paths and keep the version requirements.

For a path containing an apostrophe, use double quotes and forward slashes (for example, `"C:/src/team's/rmux/crates/rmux-sdk"`).

```toml
[dependencies]
ratatui = "0.30"
ratatui-rmux = { path = '/path/to/rmux/crates/ratatui-rmux', version = "0.11.0" }
rmux-sdk = { path = '/path/to/rmux/crates/rmux-sdk', version = "0.11.0" }
```

```rust
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use ratatui_rmux::{PaneState, PaneWidget};
use rmux_sdk::PaneSnapshot;

fn render(snapshot: PaneSnapshot, area: Rect, buffer: &mut Buffer) {
    let state = PaneState::from_snapshot(snapshot);
    PaneWidget::new(&state).render(area, buffer);
}
```

## Surface

- `PaneState` — pure data, owns a captured snapshot.
- `PaneWidget` — sync `ratatui::widgets::Widget`, safe to call from any draw loop.
- `PaneDriver` — async, pulls snapshots from the SDK and feeds them into a `PaneState`.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
