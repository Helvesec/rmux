use rmux_core::{LifecycleEvent, ManagedSessionStore, TargetFindContext};
use rmux_proto::{PaneTarget, RmuxError, WindowTarget};

use super::super::{prepare_lifecycle_event, require_expected_pane_identity, RequestHandler};
use super::queue::QueueCommandAction;
use super::tokens::CommandTokens;
use super::values::unsupported_flag;
use super::{implicit_pane_target, parse_pane_target};

mod authority;

pub(super) use authority::FloatingMoveWriteAuthority;

#[derive(Debug, Clone)]
pub(super) struct ParsedFloatingPaneMove {
    pub(super) target: PaneTarget,
    pub(super) x: Option<i32>,
    pub(super) y: Option<i32>,
}

pub(super) fn contains_position_options(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-X" | "-Y"))
}

pub(super) fn parse(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedFloatingPaneMove, RmuxError> {
    let mut target = None;
    let mut x = None;
    let mut y = None;
    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "resize-pane",
                    args.required("-t target")?,
                )?);
            }
            "-X" => {
                let _ = args.optional();
                if x.is_some() {
                    return Err(duplicate_axis("-X"));
                }
                x = Some(parse_position("-X", &args.required("-X position")?)?);
            }
            "-Y" => {
                let _ = args.optional();
                if y.is_some() {
                    return Err(duplicate_axis("-Y"));
                }
                y = Some(parse_position("-Y", &args.required("-Y position")?)?);
            }
            "-x" | "-y" | "-D" | "-L" | "-R" | "-U" | "-M" | "-T" | "-Z" => {
                return Err(RmuxError::Server(format!(
                    "resize-pane -X/-Y cannot be combined with {token}"
                )));
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("resize-pane", flag)),
            _ => break,
        }
    }
    args.no_extra("resize-pane -X/-Y")?;
    if x.is_none() && y.is_none() {
        return Err(RmuxError::Server(
            "resize-pane position requires -X or -Y".to_owned(),
        ));
    }
    Ok(ParsedFloatingPaneMove {
        target: target.unwrap_or(implicit_pane_target(sessions, find_context, "resize-pane")?),
        x,
        y,
    })
}

fn duplicate_axis(flag: &str) -> RmuxError {
    RmuxError::Server(format!("resize-pane position accepts {flag} at most once"))
}

fn parse_position(flag: &str, value: &str) -> Result<i32, RmuxError> {
    if value.ends_with('%') {
        return Err(RmuxError::Server(format!(
            "resize-pane {flag} does not accept percentages"
        )));
    }
    let position = value.parse::<i128>().map_err(|error| {
        RmuxError::Server(format!(
            "resize-pane {flag} expects a signed integer: {error}"
        ))
    })?;
    i32::try_from(position).map_err(|_| {
        RmuxError::Server(format!(
            "resize-pane {flag} position is outside signed 32-bit range"
        ))
    })
}

impl RequestHandler {
    pub(super) async fn execute_floating_pane_move(
        &self,
        command: ParsedFloatingPaneMove,
        authority: FloatingMoveWriteAuthority,
    ) -> Result<QueueCommandAction, RmuxError> {
        let window_target = WindowTarget::with_window(
            command.target.session_name().clone(),
            command.target.window_index(),
        );
        let (event, refresh_sessions) = {
            let mut state = self.state.lock().await;
            self.commit_floating_move_with_authority(&authority, &mut state, |state| {
                require_expected_pane_identity(state, &command.target)?;
                let (changed, refresh_sessions) = state
                    .mutate_window_layout_and_synchronize_family(&window_target, |mut layout| {
                        layout.translate_floating_pane(
                            command.target.pane_index(),
                            command.x,
                            command.y,
                        )
                    })?;
                let event = changed.then(|| {
                    prepare_lifecycle_event(
                        state,
                        &LifecycleEvent::WindowLayoutChanged {
                            target: window_target,
                        },
                    )
                });
                Ok((event, refresh_sessions))
            })
            .await?
        };

        if let Some(event) = event {
            self.emit_prepared(event).await;
            self.refresh_linked_window_sessions(refresh_sessions).await;
        }
        Ok(QueueCommandAction::Normal {
            output: None,
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }
}
