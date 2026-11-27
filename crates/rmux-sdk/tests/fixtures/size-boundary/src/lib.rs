//! Downstream size usage after the pre-1.0 transport conversion cutoff.

use rmux_sdk::{
    AttachSessionSpec, EnsureSession, InfoSnapshot, NewSessionSpec, Pane, Rmux, RmuxCommand,
    TerminalSizeSpec,
};

/// Applications construct SDK dimensions directly from their column and row values.
pub const SIZE: TerminalSizeSpec = TerminalSizeSpec::new(123, 45);

/// Existing SDK-only ensure, resize and info usage keeps the same signatures.
pub async fn use_size(rmux: &Rmux, pane: &Pane) -> rmux_sdk::Result<InfoSnapshot> {
    let session = rmux
        .ensure_session(EnsureSession::default().size(SIZE))
        .await?;
    pane.resize(SIZE).await?;
    let snapshot = session.window(0).info().await?;
    for window in &snapshot.windows {
        let _dimensions: (u16, u16) = (window.size.cols, window.size.rows);
    }
    Ok(snapshot)
}

/// Typed command DTOs also accept the SDK size without protocol imports.
pub fn commands(cols: u16, rows: u16) -> [RmuxCommand; 2] {
    let size = Some(TerminalSizeSpec { cols, rows });
    [
        RmuxCommand::new(NewSessionSpec {
            size,
            ..Default::default()
        }),
        RmuxCommand::new(AttachSessionSpec {
            client_size: size,
            ..Default::default()
        }),
    ]
}

#[test]
fn sdk_only_construction_preserves_literal_dimensions() {
    assert_eq!((SIZE.cols, SIZE.rows), (123, 45));
    let size = TerminalSizeSpec::new(0, 65535);
    assert_eq!((size.cols, size.rows), (0, 65535));
    assert_eq!(
        commands(size.cols, size.rows)[0].command_name(),
        "new-session"
    );
}
