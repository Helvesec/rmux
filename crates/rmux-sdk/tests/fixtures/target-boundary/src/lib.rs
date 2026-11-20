//! SDK-only usage after the deliberate pre-1.0 selector conversion migration.

use rmux_sdk::{Pane, PaneRef, Rmux, SessionName, TargetRef, Window, WindowRef};

/// Construct selectors from application data; no target-string parser is needed.
pub fn selectors(session_name: SessionName, window: u32, pane: u32) -> [TargetRef; 3] {
    [
        TargetRef::Session(session_name.clone()),
        TargetRef::Window(WindowRef::new(session_name.clone(), window)),
        TargetRef::Pane(PaneRef {
            session_name,
            window_index: window,
            pane_index: pane,
        }),
    ]
}

/// Typed handles still accept SDK selectors and expose their public components.
pub async fn handles(
    rmux: &Rmux,
    window: WindowRef,
    pane: PaneRef,
) -> rmux_sdk::Result<(Window, Pane)> {
    Ok((rmux.window(window).await?, rmux.pane(pane).await?))
}

/// Migration for logging: replace `pane.target().to_proto()` with the SDK target.
pub fn pane_label(pane: &Pane) -> String {
    pane.target().to_string()
}

#[test]
fn sdk_only_selectors_and_logging_preserve_the_existing_spelling() {
    let name = SessionName::new("work space/雪").unwrap();
    let targets = selectors(name.clone(), 0, u32::MAX);
    assert_eq!(
        targets.map(|target| target.to_string()),
        [
            "work space/雪",
            "work space/雪:0",
            "work space/雪:0.4294967295",
        ]
    );
    assert_eq!(WindowRef::first(name.clone()).window_index, 0);
    let pane = PaneRef::in_first_window(name, u32::MAX);
    assert_eq!((pane.window_index, pane.pane_index), (0, u32::MAX));
}
