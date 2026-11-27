//! C9: `display-popup` refuses sizes the attached client cannot hold, before any job runs.
//!
//! Measured on tmux 3.7b with a real attached client: a size above the client fails with
//! "width too large" / "height too large" (height checked first, the client resolved before
//! both) and runs nothing, while the same popup at an admitted size runs its job. A bordered
//! popup without room for one content cell opens nothing and still succeeds.

use std::path::Path;

use rmux_proto::{Response, RmuxError};

use super::completion_tests::{cli_command, exit_status, release_path, scene, ATTACH_PID};

/// The diagnostic a refused queued command reports, as the CLI prints it.
fn refusal(response: &Response) -> Option<String> {
    match response {
        Response::Error(error) => match &error.error {
            RmuxError::Server(message) => Some(message.clone()),
            other => Some(format!("{other:?}")),
        },
        Response::SourceFile(source) if !source.stderr().is_empty() => Some(
            String::from_utf8_lossy(source.stderr())
                .trim_end()
                .to_owned(),
        ),
        _ => None,
    }
}

fn popup_touching(size: &str, marker: &Path) -> String {
    format!(
        "display-popup -c {ATTACH_PID} {size} -E \"touch {}\"",
        marker.display()
    )
}

#[tokio::test]
async fn a_popup_larger_than_its_client_is_refused_before_its_job_runs() {
    let (handler, _control_rx) = scene("c9-refused").await;
    let refused_marker = release_path("c9-refused");
    let admitted_marker = release_path("c9-admitted");

    let refused = handler
        .handle(cli_command(popup_touching("-w 200 -h 5", &refused_marker)))
        .await;
    assert_eq!(
        refusal(&refused),
        Some("width too large".to_owned()),
        "{refused:?}"
    );
    assert!(!handler.overlay_active(ATTACH_PID).await);
    let both = handler
        .handle(cli_command(popup_touching(
            "-w 200 -h 200",
            &refused_marker,
        )))
        .await;
    assert_eq!(
        refusal(&both),
        Some("height too large".to_owned()),
        "{both:?}"
    );
    let missing_client = handler
        .handle(cli_command(format!(
            "display-popup -c 974999 -w 200 -h 5 -E \"touch {}\"",
            refused_marker.display()
        )))
        .await;
    assert!(
        refusal(&missing_client).is_some_and(|message| message.starts_with("can't find client")),
        "the client is resolved before the sizes: {missing_client:?}"
    );

    // The same job at an admitted size runs to completion in this scene, so a refused
    // popup's job had every chance to run too.
    let admitted = handler
        .handle(cli_command(popup_touching("-w 80 -h 5", &admitted_marker)))
        .await;
    assert_eq!(exit_status(&admitted), None, "{admitted:?}");
    assert!(admitted_marker.exists(), "the admitted popup ran its job");
    assert!(
        !refused_marker.exists(),
        "a refused popup never runs its job"
    );
    let _ = std::fs::remove_file(&admitted_marker);
}

#[tokio::test]
async fn a_bordered_popup_without_a_content_cell_opens_nothing() {
    let (handler, _control_rx) = scene("c9-border").await;
    let empty_marker = release_path("c9-border-empty");
    let one_cell_marker = release_path("c9-border-one-cell");

    let empty = handler
        .handle(cli_command(popup_touching("-w 2 -h 5", &empty_marker)))
        .await;
    assert_eq!(exit_status(&empty), None, "{empty:?}");
    assert!(!handler.overlay_active(ATTACH_PID).await);

    let one_cell = handler
        .handle(cli_command(popup_touching("-w 3 -h 5", &one_cell_marker)))
        .await;
    assert_eq!(exit_status(&one_cell), None, "{one_cell:?}");
    assert!(
        one_cell_marker.exists(),
        "a popup with one content cell runs its job"
    );
    assert!(
        !empty_marker.exists(),
        "the popup without a content cell ran nothing"
    );
    let _ = std::fs::remove_file(&one_cell_marker);
}
