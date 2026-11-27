mod scenario;

async fn view(pane: &rmux_sdk::Pane) -> scenario::Result {
    let snapshot: rmux_sdk::PaneSnapshot = pane.snapshot().await?;
    assert!(snapshot.visible_text().contains(scenario::MARKER));
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
    assert_eq!(scene.panes.len(), panes.len());
    assert_eq!(scene.schema, 1);
    assert_eq!(scene.revision.len(), 64);
    for pane in &scene.panes {
        assert!(panes.iter().any(|(id, _)| *id == pane.id));
    }
    println!(
        "installed owned SDK scene schema={} revision={}",
        scene.schema, scene.revision
    );
    Ok(())
}
