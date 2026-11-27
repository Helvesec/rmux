//! Atomic conversion of one live pane between tiled and floating placement.

use rmux_core::{
    command_parser::ParsedCommand, LifecycleEvent, ManagedSessionStore, TargetFindContext,
};
use rmux_proto::{PaneId, PaneTarget, RmuxError, SplitDirection, Target, WindowId, WindowTarget};

use super::super::{prepare_lifecycle_event, require_expected_pane_identity, RequestHandler};
use super::command_args::command_arguments_as_strings;
use super::floating_move::FloatingMoveWriteAuthority;
use super::queue::{QueueCommandAction, QueueInvocation};
use super::targets::{implicit_pane_target, parse_pane_target, resolve_queue_target_arguments};
use super::tokens::{normalize_compact_short_options, CommandTokens};
use crate::pane_terminal_lookup::SessionPane;
use crate::pane_terminals::{HandlerState, WindowRuntimeResizeScope};

#[derive(Debug, Clone, PartialEq, Eq)]
enum PanePlacementMode {
    Floating,
    Tiled {
        anchor: Option<PaneTarget>,
        direction: SplitDirection,
        before: bool,
        insertion_options: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PanePlacementWitness {
    source_id: PaneId,
    window_id: WindowId,
    source_floating: bool,
    anchor: Option<(PaneId, WindowId, bool)>,
    tiled_panes_present: Option<bool>,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedPanePlacement {
    pub(super) target: PaneTarget,
    mode: PanePlacementMode,
    witness: Option<PanePlacementWitness>,
}

pub(super) fn parse(
    command: ParsedCommand,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedPanePlacement, RmuxError> {
    let arguments = command_arguments_as_strings(command.name(), command.arguments())?;
    let mut args = CommandTokens::new(normalize_compact_short_options(
        "set-pane-placement",
        arguments,
    ));
    let (mut target, mut anchor, mut direction) = (None, None, None);
    let (mut before, mut before_seen) = (false, false);
    while let Some(token) = args.peek() {
        if !token.starts_with('-') || token == "-" {
            break;
        }
        let flag = args.required("option")?;
        match flag.as_str() {
            "-t" => set_once(&mut target, args.required("-t pane")?, "-t")?,
            "-a" => set_once(&mut anchor, args.required("-a tiled-neighbor")?, "-a")?,
            // The core layout primitive's variants name its divider axis,
            // while these public flags name the resulting pane arrangement.
            "-h" => set_direction(&mut direction, SplitDirection::Vertical, "-h")?,
            "-v" => set_direction(&mut direction, SplitDirection::Horizontal, "-v")?,
            "-b" if !before_seen => {
                before = true;
                before_seen = true;
            }
            "-b" => return Err(invalid("duplicate -b")),
            _ => return Err(invalid(&format!("unsupported option {flag}"))),
        }
    }

    let mode_name = args.required("floating or tiled mode")?;
    args.no_extra("set-pane-placement")?;
    let explicit_insertion = anchor.is_some() || direction.is_some() || before_seen;
    let mode = match mode_name.as_str() {
        "floating" if explicit_insertion => {
            return Err(invalid("floating mode does not accept -a, -h, -v, or -b"))
        }
        "floating" => PanePlacementMode::Floating,
        "tiled" => PanePlacementMode::Tiled {
            anchor: anchor
                .map(|value| resolve_pane("-a", value, sessions, find_context))
                .transpose()?,
            direction: direction.unwrap_or(SplitDirection::Horizontal),
            before,
            insertion_options: explicit_insertion,
        },
        _ => return Err(invalid("mode must be exactly floating or tiled")),
    };
    let target = match target {
        Some(value) => resolve_pane("-t", value, sessions, find_context)?,
        None => implicit_pane_target(sessions, find_context, "set-pane-placement")?,
    };
    Ok(ParsedPanePlacement {
        target,
        mode,
        witness: None,
    })
}

fn resolve_pane(
    flag: &str,
    value: String,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<PaneTarget, RmuxError> {
    let arguments = resolve_queue_target_arguments(
        "set-pane-placement",
        vec![flag.to_owned(), value],
        sessions,
        find_context,
    )?;
    parse_pane_target("set-pane-placement", arguments[1].clone())
}

fn set_once(slot: &mut Option<String>, value: String, flag: &str) -> Result<(), RmuxError> {
    if slot.replace(value).is_some() {
        return Err(invalid(&format!("duplicate {flag}")));
    }
    Ok(())
}

fn set_direction(
    slot: &mut Option<SplitDirection>,
    value: SplitDirection,
    flag: &str,
) -> Result<(), RmuxError> {
    if slot.replace(value).is_some() {
        return Err(invalid(&format!("duplicate or conflicting {flag}")));
    }
    Ok(())
}

fn invalid(message: &str) -> RmuxError {
    RmuxError::Server(format!("set-pane-placement: {message}"))
}

impl ParsedPanePlacement {
    pub(super) fn capture_witness(&mut self, state: &HandlerState) -> Result<(), RmuxError> {
        self.witness = Some(self.read_witness(state)?);
        Ok(())
    }

    pub(super) fn stable_targets(&self) -> impl Iterator<Item = Target> + '_ {
        std::iter::once(Target::Pane(self.target.clone()))
            .chain(self.anchor().cloned().map(Target::Pane))
    }

    fn anchor(&self) -> Option<&PaneTarget> {
        match &self.mode {
            PanePlacementMode::Floating => None,
            PanePlacementMode::Tiled { anchor, .. } => anchor.as_ref(),
        }
    }

    fn read_witness(&self, state: &HandlerState) -> Result<PanePlacementWitness, RmuxError> {
        let (source_id, window_id, source_floating, tiled_panes_present) =
            pane_placement(state, &self.target)?;
        let mut anchor_witness = None;
        let tiled_panes_present = match &self.mode {
            PanePlacementMode::Floating => None,
            PanePlacementMode::Tiled {
                insertion_options, ..
            } if !source_floating => {
                if *insertion_options {
                    return Err(invalid(
                        "insertion options cannot rearrange an already tiled pane",
                    ));
                }
                None
            }
            PanePlacementMode::Tiled {
                anchor,
                insertion_options,
                ..
            } => {
                if tiled_panes_present {
                    let anchor = anchor.as_ref().ok_or_else(|| {
                        invalid("-a tiled-neighbor is required while tiled panes exist")
                    })?;
                    let (anchor_id, anchor_window_id, anchor_floating, _) =
                        pane_placement(state, anchor)?;
                    if anchor_id == source_id {
                        return Err(invalid("source and tiled neighbor must be different"));
                    }
                    if anchor_window_id != window_id {
                        return Err(invalid("source and tiled neighbor must share one window"));
                    }
                    if anchor_floating {
                        return Err(invalid("-a must identify a tiled pane"));
                    }
                    anchor_witness = Some((anchor_id, anchor_window_id, anchor_floating));
                } else if *insertion_options {
                    return Err(invalid(
                        "-a, -h, -v, and -b are not accepted when no tiled panes remain",
                    ));
                }
                Some(tiled_panes_present)
            }
        };
        Ok(PanePlacementWitness {
            source_id,
            window_id,
            source_floating,
            anchor: anchor_witness,
            tiled_panes_present,
        })
    }

    fn require_current_witness(&self, state: &HandlerState) -> Result<(), RmuxError> {
        let expected = self
            .witness
            .as_ref()
            .ok_or_else(|| invalid("missing placement witness"))?;
        let current = self.read_witness(state)?;
        if &current == expected {
            Ok(())
        } else {
            Err(invalid("pane placement changed before execution"))
        }
    }

    fn apply(&self, store: &mut ManagedSessionStore) -> Result<bool, RmuxError> {
        let witness = self.witness.as_ref().expect("captured placement witness");
        let window_target = WindowTarget::with_window(
            self.target.session_name().clone(),
            self.target.window_index(),
        );
        let mut layout = store.window_layout_context_mut(&window_target)?;
        match &self.mode {
            PanePlacementMode::Floating if witness.source_floating => Ok(false),
            PanePlacementMode::Floating => {
                layout.float_pane_at_tiled_geometry(self.target.pane_index())?;
                Ok(true)
            }
            PanePlacementMode::Tiled { .. } if !witness.source_floating => Ok(false),
            PanePlacementMode::Tiled {
                anchor,
                direction,
                before,
                ..
            } => {
                layout.tile_floating_pane(
                    self.target.pane_index(),
                    anchor.as_ref().map(PaneTarget::pane_index),
                    *direction,
                    *before,
                )?;
                Ok(true)
            }
        }
    }
}

fn pane_placement(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<(PaneId, WindowId, bool, bool), RmuxError> {
    let window = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .ok_or_else(|| invalid("target window disappeared"))?;
    let pane = window
        .pane(target.pane_index())
        .ok_or_else(|| invalid("target pane disappeared"))?;
    let stack = window.floating_stack();
    Ok((
        pane.id(),
        window.id(),
        stack.contains(pane.id()),
        window
            .panes()
            .iter()
            .any(|candidate| !stack.contains(candidate.id())),
    ))
}

fn changed_runtime_panes(
    before: &[SessionPane],
    after: &[SessionPane],
) -> Result<Vec<PaneId>, RmuxError> {
    if before.len() != after.len()
        || before
            .iter()
            .any(|pane| !after.iter().any(|candidate| candidate.id == pane.id))
    {
        return Err(invalid("pane identity set changed during conversion"));
    }
    Ok(after
        .iter()
        .filter(|pane| {
            before
                .iter()
                .find(|candidate| candidate.id == pane.id)
                .is_some_and(|old| {
                    old.geometry.cols() != pane.geometry.cols()
                        || old.geometry.rows() != pane.geometry.rows()
                        || old.window_size != pane.window_size
                })
        })
        .map(|pane| pane.id)
        .collect())
}

impl RequestHandler {
    pub(super) async fn execute_pane_placement(
        &self,
        command: ParsedPanePlacement,
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
                if let Some(anchor) = command.anchor() {
                    require_expected_pane_identity(state, anchor)?;
                }
                command.require_current_witness(state)?;
                let before = state.window_runtime_resize_snapshot(
                    window_target.session_name(),
                    window_target.window_index(),
                )?;
                let (changed, refresh_sessions) = state
                    .mutate_store_and_resize_window_terminal_with_family_scoped(
                        window_target.session_name(),
                        window_target.window_index(),
                        |store| command.apply(store),
                        |state, changed| {
                            if !changed {
                                return Ok(None);
                            }
                            let after = state.window_runtime_resize_snapshot(
                                window_target.session_name(),
                                window_target.window_index(),
                            )?;
                            let changed_panes = changed_runtime_panes(&before, &after)?;
                            Ok((!changed_panes.is_empty())
                                .then_some(WindowRuntimeResizeScope::Panes(changed_panes)))
                        },
                    )
                    .map_err(RmuxError::from)?;
                let event = changed.then(|| {
                    prepare_lifecycle_event(
                        state,
                        &LifecycleEvent::WindowLayoutChanged {
                            target: window_target.clone(),
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

pub(super) fn as_invocation(command: ParsedPanePlacement) -> QueueInvocation {
    QueueInvocation::SetPanePlacement(command)
}
