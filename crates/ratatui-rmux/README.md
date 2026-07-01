# ratatui-rmux

Ratatui integration for the [RMUX](https://github.com/helvesec/rmux) terminal multiplexer.

Paints daemon-backed pane snapshots into a `ratatui::buffer::Buffer` with no
async work in the draw path. The async driver pulls snapshots from the
RMUX daemon; the widget is a deterministic, sync projection of the captured
state.

## Usage

```toml
[dependencies]
ratatui = "0.29"
ratatui-rmux = "0.8.0"
rmux-sdk = "0.8.0"
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
