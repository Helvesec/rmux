use rmux_proto::{SessionName, TerminalSize, WindowTarget};

use super::SessionStore;

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize { cols, rows }
}

#[test]
fn active_window_geometry_updates_terminal_and_content_dimensions() {
    let session_name = SessionName::new("active-geometry").expect("valid session name");
    let mut store = SessionStore::new();
    store
        .create_session(session_name.clone(), size(80, 24))
        .expect("session is created");

    store
        .attached_window_geometry_mut(&WindowTarget::with_window(session_name.clone(), 0))
        .expect("active geometry is borrowed")
        .resize(size(120, 40), size(120, 39));

    let session = store.session(&session_name).expect("session survives");
    assert_eq!(session.terminal_size(), size(120, 40));
    assert_eq!(
        session.window_at(0).expect("window survives").size(),
        size(120, 39)
    );
}

#[test]
fn inactive_window_geometry_preserves_terminal_dimensions() {
    let session_name = SessionName::new("inactive-geometry").expect("valid session name");
    let mut store = SessionStore::new();
    store
        .create_session(session_name.clone(), size(80, 24))
        .expect("session is created");
    let inactive_window = store
        .session_mut(&session_name)
        .expect("session exists")
        .create_window(size(90, 30))
        .expect("inactive window is created")
        .0;

    store
        .attached_window_geometry_mut(&WindowTarget::with_window(
            session_name.clone(),
            inactive_window,
        ))
        .expect("inactive geometry is borrowed")
        .resize(size(120, 40), size(120, 39));

    let session = store.session(&session_name).expect("session survives");
    assert_eq!(session.terminal_size(), size(80, 24));
    assert_eq!(
        session
            .window_at(inactive_window)
            .expect("inactive window survives")
            .size(),
        size(120, 39)
    );
}
