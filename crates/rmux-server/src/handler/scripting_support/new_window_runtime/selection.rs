use super::{HandlerState, RmuxError, StableTargetIdentity, Target, WindowTarget};

pub(super) fn find_existing_new_window_by_name(
    state: &mut HandlerState,
    session_name: &rmux_proto::SessionName,
    name: &str,
) -> Result<Option<(WindowTarget, StableTargetIdentity)>, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;
    let matches = session
        .windows()
        .filter_map(|(index, window)| (window.name() == Some(name)).then_some(index))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [window_index] => {
            let target = WindowTarget::with_window(session_name.clone(), *window_index);
            // Capture the occurrence while the name lookup is still locked.
            let identity = StableTargetIdentity::capture(state, Target::Window(target.clone()))?;
            Ok(Some((target, identity)))
        }
        _ => Err(RmuxError::Server(format!("multiple windows named {name}"))),
    }
}
