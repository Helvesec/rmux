mod scenario;
use ratatui_core::{buffer::Buffer, layout::Rect, widgets::Widget};
use ratatui_rmux::{PaneDriver, PaneWidget};

async fn view(pane: &rmux_sdk::Pane) -> scenario::Result {
    let mut driver = PaneDriver::new(pane.clone());
    driver.refresh().await?;
    let area = Rect::new(0, 0, 80, 12);
    let mut buffer = Buffer::empty(area);
    PaneWidget::new(driver.state()).render(area, &mut buffer);
    let symbols: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(symbols.contains(scenario::MARKER));
    println!("cross-facade driver and widget rendered marker");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> scenario::Result {
    scenario::run().await
}

fn view_scene(
    scene: &rmux_sdk::SceneSnapshot,
    panes: &[(rmux_sdk::PaneId, rmux_sdk::PaneSnapshot)],
) -> scenario::Result {
    let panes: Vec<_> = panes
        .iter()
        .map(|(id, p)| (*id, ratatui_rmux::PaneState::from_snapshot(p.clone())))
        .collect();
    let area = Rect::new(2, 3, scene.cols, scene.rows);
    let widget = ratatui_rmux::SceneWidget::new(scene, &panes);
    let mut first = Buffer::empty(area);
    let mut second = Buffer::empty(area);
    widget.render(area, &mut first);
    widget.render(area, &mut second);
    assert_eq!(first, second);
    assert!(scene.borders.iter().any(|b| first
        .cell((area.x + b.x, area.y + b.y))
        .unwrap()
        .symbol()
        == b.glyph.to_string()));
    for projected in &scene.panes {
        let grid = &panes
            .iter()
            .find(|(id, _)| *id == projected.id)
            .unwrap()
            .1
            .snapshot;
        for s in &projected.spans {
            let row = grid.row_cells(s.source_row as u16).unwrap();
            for c in 0..s.cols {
                let glyph = &row[s.source_col + usize::from(c)].glyph;
                if glyph.width == 1 {
                    assert_eq!(
                        first
                            .cell((area.x + s.x + c, area.y + s.y))
                            .unwrap()
                            .symbol(),
                        glyph.text
                    );
                }
            }
        }
    }
    assert!(
        widget.cursor(area).is_some(),
        "focused front pane cursor must be visible in this fixture"
    );
    println!(
        "installed Ratatui scene deterministic cursor={:?}",
        widget.cursor(area)
    );
    Ok(())
}
