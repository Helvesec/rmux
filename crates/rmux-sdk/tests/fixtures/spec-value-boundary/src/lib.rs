//! Downstream SDK-only usage. Futures are constructed, never polled.
use rmux_sdk::{
    AttachSessionSpec, ClientTerminalSpec, EnsureSession, NewSessionSpec, Pane, PaneRef,
    ProcessCommandSpec, ProcessSpec, Rmux, RmuxCommand, Session, SessionName, SplitDirection,
    SplitDirectionSpec, SplitSpec, SplitTargetSpec,
};

pub fn specs(name: SessionName) -> Vec<RmuxCommand> {
    let terminal = ClientTerminalSpec {
        terminal_features: vec!["RGB".into(), "".into()],
        utf8: true,
    };
    let targets = [
        SplitTargetSpec::Session(name.clone()),
        SplitTargetSpec::Pane(PaneRef::new(name.clone(), 0, u32::MAX)),
    ];
    let mut commands = vec![
        RmuxCommand::new(NewSessionSpec {
            session_name: Some(name),
            process: ProcessSpec::argv(["inert", ""]),
            ..Default::default()
        }),
        RmuxCommand::new(AttachSessionSpec {
            client_terminal: terminal,
            ..Default::default()
        }),
    ];
    for target in targets {
        for direction in [SplitDirectionSpec::Vertical, SplitDirectionSpec::Horizontal] {
            commands.push(RmuxCommand::new(SplitSpec {
                target: target.clone(),
                direction,
                before: false,
                process: ProcessSpec::shell(""),
            }));
        }
    }
    commands
}

pub fn builders(rmux: &Rmux, session: &Session, pane: &Pane) {
    drop(rmux.ensure_session(
        EnsureSession::named(session.name().clone()).process(ProcessSpec::shell("inert")),
    ));
    let _window = session
        .new_window_with()
        .spawn(["inert", ""])
        .shell("inert");
    let _split = pane
        .split_with(SplitDirection::Right)
        .spawn(["inert"])
        .shell("inert");
    let _spawn = pane.spawn(["inert", ""]);
    let _shell = pane.shell("inert");
}

pub fn source_mode(value: &ProcessCommandSpec) -> &str {
    match value {
        ProcessCommandSpec::Argv(_) => "argv",
        ProcessCommandSpec::Shell(_) => "shell",
        _ => "future SDK mode",
    }
}

#[test]
fn values_remain_inert_and_sdk_native() {
    let name = SessionName::new(" unusual $@%[];=/é雪 ").unwrap();
    let commands = specs(name);
    assert_eq!(commands.len(), 6);
    assert_eq!(commands[0].command_name(), "new-session");
    assert_eq!(commands[1].command_name(), "attach-session");
    assert_eq!(commands[2], commands[2].clone());
    assert_eq!(
        ClientTerminalSpec::default().terminal_features,
        Vec::<String>::new()
    );
    assert!(!ClientTerminalSpec::default().utf8);
    assert_eq!(SplitDirectionSpec::default(), SplitDirectionSpec::Vertical);
    let mut set = std::collections::HashSet::new();
    set.insert(SplitDirectionSpec::Horizontal);
    set.insert(SplitDirectionSpec::Horizontal);
    assert_eq!(set.len(), 1);
    assert_eq!(
        source_mode(&ProcessCommandSpec::Shell(String::new())),
        "shell"
    );
    assert!(!format!("{:?}", commands).is_empty());
}
