use super::fpc1_junction::fixture as junction_fixture;
use super::frs1::{cleanup, queue, window};
use super::*;
use std::fs;

#[tokio::test]
async fn requester_control_current_precedes_stale_inherited_source_file_target() {
    let (handler, primary, linked, root, requester, _events) = junction_fixture().await;
    let requester_pid = requester.0.id();
    let control_id = handler
        .control_queue_client_id(requester_pid)
        .await
        .expect("fixture requester is a registered control");
    handler
        .set_control_session(requester_pid, Some(primary.clone()))
        .await
        .expect("requester control attaches to primary session");

    let inherited_pane = window(&handler, &primary)
        .await
        .pane(0)
        .expect("inherited pane")
        .id();
    queue(
        &handler,
        "swap-pane -d -s frs-sibling-primary:0.0 -t junction-decoy:0.0",
    )
    .await
    .expect("administrative swap makes inherited pane stale");

    let attached_current = window(&handler, &primary)
        .await
        .active_pane()
        .expect("attached control current pane")
        .id();
    assert_ne!(attached_current, inherited_pane);
    assert!(
        window(&handler, &linked)
            .await
            .panes()
            .iter()
            .all(|pane| pane.id() != inherited_pane),
        "the requester's inherited SessionId/PaneId pair must be stale"
    );
    let decoy = session_name("junction-decoy");
    assert_eq!(
        window(&handler, &decoy)
            .await
            .pane(0)
            .expect("destination pane")
            .id(),
        inherited_pane
    );

    let source = root.join("requester-control.conf");
    fs::write(&source, "swap-pane -d -t junction-decoy:0.0\n").expect("write source-file fixture");
    let commands = handler
        .parse_control_commands(&format!("source-file {}", source.display()))
        .await
        .expect("public control source-file parses");
    let result = handler
        .execute_control_commands_identity(requester_pid, control_id, commands)
        .await;

    assert!(result.error.is_none(), "{result:?}");
    assert_eq!(result.exit_status, None, "{result:?}");
    assert_eq!(
        window(&handler, &decoy)
            .await
            .pane(0)
            .expect("destination after source-file")
            .id(),
        attached_current,
        "the live current of the requester control is the implicit source"
    );

    cleanup(&handler).await;
    drop(requester);
    fs::remove_dir_all(root).expect("remove requester-control fixture");
}
