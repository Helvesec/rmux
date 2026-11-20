//! FNP1's explicit placement admission. No temporary tile or split RPC.
use std::path::PathBuf;

use rmux_core::{
    command_parser::ParsedCommand, FloatingPaneRequest, ManagedSessionStore, TargetFindContext,
};
use rmux_proto::{
    DisplayMessageRequest, PaneTarget, Response, RmuxError, SplitDirection, SplitWindowTarget,
    Target,
};

mod defaults;
mod dimensions;
mod positions;

use self::dimensions::Dimension;
use self::positions::Position;
use super::command_args::command_arguments_as_strings;
use super::queue::{queue_action_from_response, QueueCommandAction, QueueExecutionContext};
use super::targets::{implicit_pane_target, parse_pane_target, resolve_queue_target_arguments};
use super::tokens::{normalize_compact_short_options, CommandTokens};
use super::RequestHandler;
use crate::handler::attach_support::{ActiveAttachIdentity, ActiveAttachState, ClientFlags};
use crate::handler::pane_support::{PaneStdinPolicy, SplitWindowParts, SplitWindowResponseMode};
use crate::handler::RequesterOrigin;
use crate::pane_terminals::SplitExitPolicyOverride;

tokio::task_local! {
    static CREATION_AUTHORITY: (RequesterOrigin, Option<ActiveAttachIdentity>);
}

pub(in crate::handler) struct NewPaneCommitAccess<'a> {
    pub(in crate::handler) attach: tokio::sync::MutexGuard<'a, ActiveAttachState>,
    _control: tokio::sync::MutexGuard<'a, crate::handler::control_support::ActiveControlState>,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedNewPane {
    pub(super) target: PaneTarget,
    rect: FloatingPaneRequest,
    detached: bool,
    /// The pane is created with no process at all, `new-pane -E` or `-I`.
    empty: bool,
    /// `new-pane -I` was written, so this opening has a channel to read and is
    /// worth nothing without it. The source makes `-I` imply an empty pane and
    /// then reads the invoking client's standard input
    /// (`cmd-split-window.c:103-115` and `window.c:1789-1835`).
    requires_input: bool,
    /// The finished bytes of that channel, installed under the lock that admits
    /// the command. `Some(empty)` is an end of file without a byte, which is
    /// not the same thing as `None`, where no channel reached this command.
    stdin_payload: Option<Vec<u8>>,
    /// `new-pane -k`: the float is kept when its command ends, until an
    /// ordinary key of an attached client closes it. The source writes this as
    /// the pane's own `remain-on-exit` (`cmd-split-window.c:192-197`).
    keep_until_key: bool,
    command: Option<Vec<String>>,
    directory: Option<PathBuf>,
    environment: Vec<String>,
    print: bool,
    format: Option<String>,
}

pub(super) fn parse(
    command: ParsedCommand,
    sessions: &ManagedSessionStore,
    context: &TargetFindContext,
) -> Result<ParsedNewPane, RmuxError> {
    let arguments = command_arguments_as_strings(command.name(), command.arguments())?;
    let mut args = CommandTokens::new(normalize_compact_short_options("new-pane", arguments));
    // Sizes and positions both stay typed until the target's window is known,
    // because either may be written as a percentage of it.
    let mut sizes: [Option<Dimension>; 2] = [None; 2];
    let mut positions: [Option<Position>; 2] = [None; 2];
    let (mut target, mut directory, mut format) = (None, None, None);
    let (mut detached, mut print, mut empty) = (false, false, false);
    let mut requires_input = false;
    let mut keep_until_key = false;
    let mut environment = Vec::new();
    while let Some(token) = args.peek() {
        if token == "--" {
            args.optional();
            break;
        }
        if !token.starts_with('-') {
            break;
        }
        let flag = args.required("option")?;
        match flag.as_str() {
            "-d" => detached = true,
            "-P" => print = true,
            "-E" => empty = true,
            "-I" => {
                // The source reads this flag before anything is placed and lets
                // it imply the empty pane, so a command written beside it is
                // refused by the same diagnostic `-E` has.
                empty = true;
                requires_input = true;
            }
            "-k" => keep_until_key = true,
            "-c" => directory = Some(PathBuf::from(args.required("-c directory")?)),
            "-F" => format = Some(args.required("-F format")?),
            "-e" => environment.push(args.required("-e NAME=VALUE")?),
            "-t" => {
                if target.is_some() {
                    return Err(invalid("duplicate -t"));
                }
                target = Some(args.required("-t pane")?);
            }
            "-x" | "-y" => {
                let index = usize::from(flag == "-y");
                if sizes[index].is_some() {
                    return Err(invalid("duplicate geometry"));
                }
                let value = args.required(&flag)?;
                sizes[index] = Some(Dimension::parse(&flag, &value)?);
            }
            "-X" | "-Y" => {
                let index = usize::from(flag == "-Y");
                if positions[index].is_some() {
                    return Err(invalid("duplicate geometry"));
                }
                let value = args.required(&flag)?;
                positions[index] = Some(Position::parse(&flag, &value)?);
            }
            _ => return Err(invalid(&format!("unsupported option {flag}"))),
        }
    }
    if format.is_some() && !print {
        return Err(invalid("-F requires -P"));
    }
    crate::terminal::parse_environment_assignments(&environment)?;
    // What may follow the options is decided here, before the target and the
    // rectangle are resolved, because tmux decides it there too: `-E` refuses
    // a command before it places the cell (`cmd-split-window.c:103-115`).
    let command = (!args.is_empty()).then(|| args.remaining());
    let command = if empty {
        // tmux accepts no command at all, or exactly one argument that is the
        // empty string, and refuses everything else with this diagnostic.
        match command.as_deref() {
            None => None,
            Some([only]) if only.is_empty() => None,
            Some(_) => return Err(invalid("command cannot be given for empty pane")),
        }
    } else {
        if command.as_ref().is_some_and(|args| args[0].is_empty()) {
            return Err(invalid("empty command is not supported"));
        }
        command
    };
    // A position may be omitted on either axis. What fills it is the window's
    // own memory of where it last placed a float (`layout.c:1399-1417`), so the
    // request travels unresolved from here and the window decides, under the
    // borrow that creates the pane.
    let [x, y] = positions;
    let [width, height] = sizes;
    // tmux declares this target `{ 't', CMD_FIND_PANE, 0 }`, so an omitted `-t`
    // is the current pane, found the way every other command finds it. The
    // resolver and the queue's stable identities are the ones already in use by
    // `resize-pane`, `send-keys` and `select-pane`; nothing new resolves here.
    let target = match target.filter(|value| !value.is_empty()) {
        Some(target) => {
            let resolved = resolve_queue_target_arguments(
                "new-pane",
                vec!["-t".into(), target],
                sessions,
                context,
            )?;
            parse_pane_target("new-pane", resolved[1].clone())?
        }
        None => implicit_pane_target(sessions, context, "new-pane")?,
    };
    // A percentage is of the window that owns the resolved target, never of the
    // client or of the target pane. This runs under the state lock that admits
    // the command, so it reads the size the queue has reached, not the size the
    // queue started with. Positions and sizes share that one borrow.
    let window = sessions
        .resolve_window(&Target::Pane(target.clone()))?
        .size();
    // An omitted axis takes its default from that same window size, so a
    // resize admitted earlier in this queue is what it reads. The two axes are
    // resolved independently, and a default that truncates to zero reaches the
    // rectangle unrescued, where it is refused before anything is created.
    let rect = FloatingPaneRequest::new(
        x.map(|x| x.resolve(window.cols)),
        y.map(|y| y.resolve(window.rows)),
        width.map_or_else(
            || defaults::width(window.cols),
            |width| width.resolve(window.cols),
        ),
        height.map_or_else(
            || defaults::height(window.rows),
            |height| height.resolve(window.rows),
        ),
    )?;
    Ok(ParsedNewPane {
        target,
        rect,
        detached,
        empty,
        requires_input,
        stdin_payload: None,
        keep_until_key,
        command,
        directory,
        environment,
        print,
        format,
    })
}

impl ParsedNewPane {
    /// Installs the finished bytes offered with this command, under the lock.
    ///
    /// Only an opening that asked for them accepts them: a payload offered to a
    /// command without `-I` has no pane to reach and is refused here, before
    /// the target identity is captured and before anything is created.
    pub(super) fn install_stdin_payload(&mut self, payload: Vec<u8>) -> Result<(), RmuxError> {
        if !self.requires_input {
            return Err(invalid("input requires -I"));
        }
        self.stdin_payload = Some(payload);
        Ok(())
    }
}

fn invalid(message: &str) -> RmuxError {
    RmuxError::Server(format!("new-pane: {message}"))
}

impl RequestHandler {
    pub(super) async fn with_new_pane_authority<T>(
        &self,
        pid: u32,
        floating: bool,
        execution: impl std::future::Future<Output = T>,
    ) -> T {
        if !floating {
            return execution.await;
        }
        let origin = self.capture_requester_origin(pid).await;
        let identity = self
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&pid)
            .map(|active| active.identity(pid));
        CREATION_AUTHORITY
            .scope((origin, identity), execution)
            .await
    }

    pub(in crate::handler) async fn lock_new_pane_commit(
        &self,
        state: &crate::pane_terminals::HandlerState,
    ) -> Result<NewPaneCommitAccess<'_>, RmuxError> {
        let (origin, identity) = CREATION_AUTHORITY
            .try_with(Clone::clone)
            .map_err(|_| invalid("missing creation authority"))?;
        let active = self.active_attach.lock().await;
        let control = self.active_control.lock().await;
        if let Some(identity) =
            crate::handler::control_support::current_control_queue_identity(origin.requester_pid())
        {
            Self::validate_control_queue_identity_locked(
                state,
                &control,
                identity.requester_pid(),
                identity.control_id(),
            )?;
            if !control
                .by_pid
                .get(&origin.requester_pid())
                .is_some_and(|client| client.can_write)
            {
                return Err(invalid("control client is read-only"));
            }
        }
        let _access = self.require_requester_origin_write(&origin).await?;
        if let Some(identity) = identity {
            if !active
                .by_pid
                .get(&origin.requester_pid())
                .is_some_and(|client| {
                    identity.matches_active(client)
                        && !client.closing.load(std::sync::atomic::Ordering::SeqCst)
                        && client.can_write
                        && !client.flags.contains(ClientFlags::READONLY)
                })
            {
                return Err(invalid("origin attach replaced or read-only"));
            }
        }
        Ok(NewPaneCommitAccess {
            attach: active,
            _control: control,
        })
    }

    pub(super) async fn execute_new_pane(
        &self,
        requester_pid: u32,
        command: ParsedNewPane,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let mut source_input = match (
            command.requires_input && command.stdin_payload.is_none(),
            context.invocation_input.as_ref(),
        ) {
            (true, Some(input)) => Some(input.reserve().await),
            _ => None,
        };
        let payload = command.stdin_payload.or_else(|| {
            source_input
                .as_ref()
                .and_then(|reservation| reservation.as_ref().cloned())
        });
        if command.requires_input && payload.is_none() {
            return Err(invalid("-I requires the finished input of an invocation"));
        }
        let stdin_payload = payload.or_else(|| command.empty.then(Vec::new));
        let stdin_policy = if command.requires_input {
            PaneStdinPolicy::FloatingRaw
        } else {
            PaneStdinPolicy::SplitNormalized
        };
        let response = self
            .handle_split_window_parts(
                requester_pid,
                SplitWindowParts {
                    target: SplitWindowTarget::Pane(command.target),
                    expected_pane_id: None,
                    floating: Some(command.rect),
                    direction: SplitDirection::Vertical,
                    before: false,
                    environment_overrides: Some(command.environment),
                    command: command.command,
                    process_command: None,
                    start_directory: command.directory.or_else(|| context.caller_cwd.clone()),
                    // The pane's own `remain-on-exit`, set by the commit path
                    // before the process is launched: a command that has
                    // already finished must still find the policy in place.
                    keep_alive_on_exit: command
                        .keep_until_key
                        .then_some(SplitExitPolicyOverride::Key),
                    detached: command.detached,
                    size: None,
                    preserve_zoom: false,
                    full_size: false,
                    // An empty payload beside no command is how this server
                    // already says "create the pane with no process": the
                    // dispatch reads exactly that pair to pass
                    // `SplitPaneProcess::Empty` and to skip `default-command`.
                    // `-I` fills the same field with what it read, and its
                    // policy says the bytes reach the parser as they were read.
                    stdin_payload,
                    stdin_policy,
                    response_mode: SplitWindowResponseMode::StableIdentity,
                },
            )
            .await;
        let Response::SplitWindowIdentity(created) = &response else {
            return queue_action_from_response(response);
        };
        // Creation has committed with its target and write authority rechecked.
        // Printing may still fail, but the bytes now belong to the created pane.
        if let Some(reservation) = source_input.as_mut() {
            reservation.take();
        }
        drop(source_input);
        if !command.print {
            return queue_action_from_response(response);
        }
        #[cfg(test)]
        self.pause_before_new_pane_print_for_test().await;
        // The source identity authorizes creation, not a read of its new child.
        let printed = super::super::without_expected_stable_target_identities(
            self.handle_display_message_for_stable_pane(
                requester_pid,
                created.pane_id,
                DisplayMessageRequest {
                    target: Some(Target::Pane(created.pane.clone())),
                    print: true,
                    message: Some(command.format.unwrap_or_else(|| {
                        "#{session_name}:#{window_index}.#{pane_index}".to_owned()
                    })),
                    empty_target_context: false,
                },
            ),
        )
        .await;
        queue_action_from_response(printed)
    }
}
