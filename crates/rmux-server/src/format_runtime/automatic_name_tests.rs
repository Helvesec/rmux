use super::*;
use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
use crate::pane_terminals::HandlerState;
use rmux_proto::{
    OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
};

// LBseed and the explicit E fixed point are frozen by the native tmux 3.7b
// reference scenario in tests/client_loop_reference/reentry.py.
fn fixture(template: &str, enabled: bool) -> (HandlerState, SessionName) {
    let mut state = HandlerState::default();
    let name = SessionName::new("N").unwrap();
    state
        .sessions
        .create_session(name.clone(), TerminalSize { cols: 80, rows: 24 })
        .unwrap();
    let target = WindowTarget::with_window(name.clone(), 0);
    state
        .sessions
        .set_automatic_window_name(&target, "LBseed".to_owned())
        .unwrap();
    for (option, value) in [
        (OptionName::AutomaticRenameFormat, template),
        (
            OptionName::AutomaticRename,
            if enabled { "on" } else { "off" },
        ),
    ] {
        state
            .options
            .set(
                ScopeSelector::Window(target.clone()),
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    (state, name)
}

fn check(template: &str, enabled: bool, attached: bool, expected: &str) {
    let (state, name) = fixture(template, enabled);
    let session = state.sessions.session(&name).unwrap();
    let window = session.window();
    let clients = [ClientLoopSnapshot {
        bindings: ClientFormatBindings {
            name: "C".to_owned(),
            session: "N".to_owned(),
            ..ClientFormatBindings::default()
        },
        order: 0,
        activity_at: crate::client_activity::ClientActivityTimestamp::from_parts(0, 0),
    }];
    let base = FormatContext::new()
        .with_managed_session(session)
        .with_window(0, window, true, false);
    let runtime = RuntimeFormatContext::new(base)
        .with_state(&state)
        .with_session(session)
        .with_window(0, window)
        .with_client_loop_snapshots(if attached { &clients } else { &[] });
    assert_eq!(
        render_runtime_template(
            "AUTO=#{E:automatic-rename-format}|NAME=#{window_name}|END",
            &runtime,
            false
        ),
        expected
    );
    assert_eq!(window.name(), Some("LBseed"));
    assert_eq!(runtime.format_name_exists(None, "LBseed"), Some(true));
}

#[test]
fn automatic_name_client_loop_reads_stored_name() {
    check(
        "#{L:#{window_name}}",
        true,
        true,
        "AUTO=LBseed|NAME=LBseed|END",
    );
}

#[test]
fn automatic_name_client_loop_neighbors_keep_context_and_expansion_guard() {
    for template in [
        "#{window_name}",
        "#{L:#{L:#{window_name}}}",
        "#{L:#{W:#{window_name}}}",
        "#{S:#{L:#{window_name}}}",
        "#{L:#{P:#{window_name}}}",
    ] {
        check(template, true, true, "AUTO=LBseed|NAME=LBseed|END");
    }
    check(
        "#{L:#{window_name}}",
        false,
        true,
        "AUTO=LBseed|NAME=LBseed|END",
    );
    check("#{L:#{window_name}}", true, false, "AUTO=|NAME=LBseed|END");
    check(
        "#{L:#{E:automatic-rename-format}}",
        true,
        true,
        "AUTO=|NAME=LBseed|END",
    );
}
