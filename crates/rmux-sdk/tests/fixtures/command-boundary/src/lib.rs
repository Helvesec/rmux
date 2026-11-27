//! Supported downstream construction and execution signatures, SDK only.
use rmux_sdk::{
    AttachSessionSpec, CommandRun, EnsureSession, NewSessionSpec, Pane, PaneRef, ProcessSpec,
    RefreshClientSpec, Rmux, RmuxCommand, RmuxCommandKind, RmuxEndpoint, Session, SessionName,
    SplitDirection, SplitDirectionSpec, SplitSpec, SplitTargetSpec,
};

pub fn commands(name: SessionName) -> [RmuxCommand; 4] {
    [
        NewSessionSpec {
            session_name: Some(name.clone()),
            process: ProcessSpec::argv(["inert", ""]),
            ..Default::default()
        }
        .into(),
        RmuxCommandKind::from(AttachSessionSpec::default()).into(),
        RmuxCommand::with_endpoint(
            RmuxEndpoint::WindowsPipe("inert".into()),
            SplitSpec {
                target: SplitTargetSpec::Pane(PaneRef::new(name, u32::MAX, 0)),
                direction: SplitDirectionSpec::Horizontal,
                before: true,
                process: ProcessSpec::shell(" $X; é雪 "),
            },
        ),
        RmuxCommand::new(RefreshClientSpec::default()),
    ]
}

// Compile the actual public execution paths; tests never poll these futures.
pub async fn execute(rmux: &Rmux, session: &Session, pane: &Pane) -> rmux_sdk::Result<CommandRun> {
    rmux.ensure_session(EnsureSession::named(session.name().clone()).detached(true))
        .await?;
    session.new_window_with().spawn(["inert", ""]).await?;
    pane.split_with(SplitDirection::Right)
        .shell("inert")
        .await?;
    pane.spawn(["inert", ""]).await?;
    rmux.cmd(["display-message", "-p", "literal ; $X"]).await
}

#[cfg(test)]
fn all_conversions<T: Clone + Into<RmuxCommandKind> + Into<RmuxCommand>>(value: T) {
    let kind: RmuxCommandKind = value.clone().into();
    let direct: RmuxCommand = value.into();
    assert_eq!(direct, RmuxCommand::new(kind));
}

#[test]
fn public_values_are_typed_and_inert() {
    let values = commands(SessionName::new("alpha").unwrap());
    assert_eq!(
        values.map(|value| value.command_name()),
        [
            "new-session",
            "attach-session",
            "split-window",
            "refresh-client"
        ]
    );
    all_conversions(NewSessionSpec::default());
    all_conversions(AttachSessionSpec::default());
    all_conversions(RefreshClientSpec::default());
    all_conversions(SplitSpec {
        target: SplitTargetSpec::Session(SessionName::new("alpha").unwrap()),
        direction: SplitDirectionSpec::Vertical,
        before: false,
        process: ProcessSpec::default(),
    });
}
