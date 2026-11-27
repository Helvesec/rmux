#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;

use rmux_core::{
    command_target_metadata, CommandTargetSpec, ManagedSessionStore, MissingCurrentTargetFallback,
    OptionStore, TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use rmux_proto::{
    MoveWindowTarget, PaneTarget, RmuxError, SelectLayoutTarget, SessionName, SplitWindowTarget,
    Target, WindowTarget,
};

use super::super::target_support::{inherited_pane_target, RequesterEnvironmentContext};
use super::tokens::{
    normalize_repeated_target_options, short_option_prefix_len, short_option_takes_next_value,
};
use super::values::missing_argument;

mod historical;
mod independence;
mod new_window;
mod queue_arguments;
pub(super) use historical::admit_historical_target_roles;
pub(super) use independence::command_has_independent_target;
pub(super) use new_window::{
    parse_new_window_target_argument, parse_queued_new_window_target_argument, NewWindowTargetIndex,
};
pub(in crate::handler::scripting_support) use queue_arguments::bare_link_window_destination_candidate;
use queue_arguments::{
    preserve_link_window_bare_destination_value, queue_target_spec_for_flag,
    short_flag_argument_parts,
};
pub(super) use queue_arguments::{
    resolve_queue_target_arguments, signed_window_target_session_part,
};

pub(super) fn resolve_queue_target_argument(
    command_name: &str,
    flag: char,
    value: String,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<String, RmuxError> {
    let Some(spec) = target_spec_for_flag(command_name, flag) else {
        return Ok(value);
    };
    resolve_target_argument_with_spec(value, spec, sessions, find_context)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QueueTargetArgumentResolution {
    Resolved(String),
    CanFail,
}

pub(super) fn resolve_queue_target_argument_typed(
    command_name: &str,
    flag: char,
    value: String,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<QueueTargetArgumentResolution, RmuxError> {
    let Some(spec) = target_spec_for_flag(command_name, flag) else {
        return Ok(QueueTargetArgumentResolution::Resolved(value));
    };
    resolve_target_argument_with_spec_typed(value, spec, sessions, find_context)
}

fn resolve_target_argument_with_spec_typed(
    value: String,
    spec: CommandTargetSpec,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<QueueTargetArgumentResolution, RmuxError> {
    match sessions.resolve_unresolved_target(
        &UnresolvedTarget::new(value),
        spec.find_type,
        spec.flags,
        find_context,
    ) {
        Ok(target) => Ok(QueueTargetArgumentResolution::Resolved(target.to_string())),
        Err(_) if spec.flags.contains(TargetFindFlags::CANFAIL) => {
            Ok(QueueTargetArgumentResolution::CanFail)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn resolve_target_argument_with_spec(
    value: String,
    spec: CommandTargetSpec,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<String, RmuxError> {
    let target = sessions.resolve_unresolved_target(
        &UnresolvedTarget::new(value.clone()),
        spec.find_type,
        spec.flags,
        find_context,
    )?;

    Ok(target.to_string())
}

fn target_spec_for_flag(command_name: &str, flag: char) -> Option<CommandTargetSpec> {
    let metadata = command_target_metadata(command_name)?;
    [metadata.source, metadata.target]
        .into_iter()
        .flatten()
        .find(|spec| spec.flag == flag)
}

pub(super) struct QueueTargetFindContextInput<'a> {
    pub(super) sessions: &'a ManagedSessionStore,
    pub(super) options: &'a OptionStore,
    pub(super) requester_environment: RequesterEnvironmentContext,
    pub(super) attached_session: Option<&'a SessionName>,
    pub(super) requester_owns_attached_session: bool,
    pub(super) current_target: Option<&'a Target>,
    /// The current target stands for the requesting client, not a pinned
    /// queue state; see [`TargetFindContext::following_client`].
    pub(super) current_target_follows_client: bool,
    pub(super) missing_current_target_fallback: MissingCurrentTargetFallback,
    pub(super) mouse_target: Option<&'a Target>,
    pub(super) marked_target: Option<&'a PaneTarget>,
}

pub(super) fn queue_target_find_context(
    input: QueueTargetFindContextInput<'_>,
) -> Result<TargetFindContext, RmuxError> {
    let context = if let Some(current_target) = input.current_target {
        let context = TargetFindContext::from_target(current_target.clone());
        if input.current_target_follows_client {
            context.following_client()
        } else {
            context
        }
    } else {
        let attached_target = input
            .attached_session
            .and_then(|session_name| active_session_target(input.sessions, session_name));
        let current = if input.requester_owns_attached_session && attached_target.is_some() {
            attached_target
        } else {
            inherited_pane_target(input.sessions, input.requester_environment, None)?
                .or(attached_target)
        }
        .or_else(|| latest_detached_session_target(input.sessions));
        TargetFindContext::new(current).following_client()
    };

    let context = match input.missing_current_target_fallback {
        MissingCurrentTargetFallback::AllowDefaultSession => context,
        MissingCurrentTargetFallback::ForbidDefaultSession => {
            context.forbid_missing_current_target_fallback()
        }
    };
    let context = context
        .with_mouse_target(input.mouse_target.cloned())
        .with_marked_target(input.marked_target.cloned().map(Target::Pane));
    Ok(crate::handler::with_visible_pane_bases(
        context,
        input.sessions,
        input.options,
    ))
}

fn latest_detached_session_target(sessions: &ManagedSessionStore) -> Option<Target> {
    sessions
        .iter()
        .map(|session| (session.name(), session))
        .max_by(|(left_name, left_session), (right_name, right_session)| {
            left_session
                .recency()
                .cmp(&right_session.recency())
                .then(left_session.id().cmp(&right_session.id()))
                .then(right_name.as_str().cmp(left_name.as_str()))
        })
        .and_then(|(session_name, _)| active_session_target(sessions, session_name))
}

pub(super) fn active_session_target(
    sessions: &ManagedSessionStore,
    session_name: &SessionName,
) -> Option<Target> {
    let session = sessions.session(session_name)?;
    let window_index = session.active_window_index();
    let pane_index = session
        .window_at(window_index)
        .and_then(rmux_core::Window::active_pane)
        .map(rmux_core::Pane::index)?;
    Some(Target::Pane(PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane_index,
    )))
}

fn implicit_target_for_type(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    find_type: TargetFindType,
    command_name: &str,
) -> Result<Target, RmuxError> {
    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::none(),
            find_type,
            TargetFindFlags::NONE,
            find_context,
        )
        .map_err(|_| missing_argument(command_name, "-t target"))
}

pub(super) fn implicit_session_name(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<SessionName, RmuxError> {
    match implicit_target_for_type(
        sessions,
        find_context,
        TargetFindType::Session,
        command_name,
    )? {
        Target::Session(session_name) => Ok(session_name),
        _ => unreachable!("session target lookup must return a session"),
    }
}

pub(super) fn implicit_window_target(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<WindowTarget, RmuxError> {
    match implicit_target_for_type(sessions, find_context, TargetFindType::Window, command_name)? {
        Target::Window(target) => Ok(target),
        _ => unreachable!("window target lookup must return a window"),
    }
}

pub(super) fn implicit_pane_target(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<PaneTarget, RmuxError> {
    match implicit_target_for_type(sessions, find_context, TargetFindType::Pane, command_name)? {
        Target::Pane(target) => Ok(target),
        _ => unreachable!("pane target lookup must return a pane"),
    }
}

pub(super) fn marked_pane_target(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<PaneTarget, RmuxError> {
    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::new("{marked}"),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            find_context,
        )
        .map(|target| match target {
            Target::Pane(target) => target,
            _ => unreachable!("marked pane target lookup must return a pane"),
        })
        .map_err(|error| {
            RmuxError::Server(format!("{command_name} requires a marked pane: {error}"))
        })
}

pub(super) fn marked_pane_target_or_current(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<PaneTarget, RmuxError> {
    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::none(),
            TargetFindType::Pane,
            TargetFindFlags::DEFAULT_MARKED,
            find_context,
        )
        .map(|target| match target {
            Target::Pane(target) => target,
            _ => unreachable!("default marked pane lookup must return a pane"),
        })
        .map_err(|_| missing_argument(command_name, "-s target"))
}

pub(super) fn implicit_split_target(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<SplitWindowTarget, RmuxError> {
    let target = implicit_pane_target(sessions, find_context, command_name)?;
    Ok(SplitWindowTarget::Pane(target))
}

/// Parses a name for a session being created or renamed; a literal `\` is
/// escaped as tmux's `session_check_name` does.
pub(super) fn parse_session_name(value: String) -> Result<SessionName, RmuxError> {
    SessionName::new(value).map_err(|_| RmuxError::EmptySessionName)
}

/// Parses `-t`/`-s` text that looks up an existing session: the text is read
/// as the canonical stored name, so it is not escaped a second time.
pub(super) fn parse_session_target(value: String) -> Result<SessionName, RmuxError> {
    SessionName::from_canonical(value).map_err(|_| RmuxError::EmptySessionName)
}

pub(super) fn parse_target_arg(command: &str, value: String) -> Result<Target, RmuxError> {
    Target::parse(&value)
        .map_err(|error| RmuxError::Server(format!("invalid {command} target '{value}': {error}")))
}

pub(super) fn parse_window_target(command: &str, value: String) -> Result<WindowTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Window(target)) => Ok(target),
        Ok(_) => Err(RmuxError::Server(format!(
            "{command} target must match 'session:window'"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_pane_target(_command: &str, value: String) -> Result<PaneTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Pane(target)) => Ok(target),
        Ok(Target::Window(target)) => Ok(PaneTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
            0,
        )),
        Ok(Target::Session(session_name)) => Ok(PaneTarget::with_window(session_name, 0, 0)),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_move_window_target(value: String) -> Result<MoveWindowTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok(MoveWindowTarget::Session(session_name)),
        Ok(Target::Window(target)) => Ok(MoveWindowTarget::Window(target)),
        Ok(Target::Pane(_)) => Err(RmuxError::Server(format!(
            "invalid move-window target '{value}': move-window target must match 'session' or 'session:window'"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_split_window_target(value: String) -> Result<SplitWindowTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok(SplitWindowTarget::Session(session_name)),
        Ok(Target::Pane(target)) => Ok(SplitWindowTarget::Pane(target)),
        Ok(Target::Window(_)) => Err(RmuxError::Server(format!(
            "invalid split-window target '{value}': split-window requires a session or pane target"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_select_layout_target(value: String) -> Result<SelectLayoutTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok(SelectLayoutTarget::Session(session_name)),
        Ok(Target::Window(target)) => Ok(SelectLayoutTarget::Window(target)),
        Ok(Target::Pane(_)) => Err(RmuxError::Server(format!(
            "invalid select-layout target '{value}': select-layout requires a session or window target"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_layout_name(value: &str) -> Result<rmux_proto::LayoutName, RmuxError> {
    value.parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_display_target_is_independent_of_implicit_context() {
        let parsed = rmux_core::command_parser::CommandParser::new()
            .parse("display-message -p -t primary:0.1 '#{pane_id}'")
            .expect("display-message parses");
        let command = parsed.commands().first().expect("one command");

        assert!(command_has_independent_target(command, None));
    }

    fn has_independent_target(command: &str) -> bool {
        let parsed = rmux_core::command_parser::CommandParser::new()
            .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
            .parse(command)
            .expect("command parses");
        command_has_independent_target(parsed.commands().first().expect("one command"), None)
    }

    fn has_independent_target_with_mark(command: &str) -> bool {
        let parsed = rmux_core::command_parser::CommandParser::new()
            .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
            .parse(command)
            .expect("command parses");
        let marked = PaneTarget::with_window(session_name("marked"), 0, 0);
        command_has_independent_target(
            parsed.commands().first().expect("one command"),
            Some(&marked),
        )
    }

    #[test]
    fn independent_target_classification_uses_last_target_precedence() {
        assert!(!has_independent_target(
            "resize-pane -t primary:0.1 -t . -x31"
        ));
        assert!(has_independent_target(
            "resize-pane -t . -t primary:0.1 -x31"
        ));
        assert!(has_independent_target(
            "resize-pane -t . -tprimary:0.1 -x31"
        ));
    }

    #[test]
    fn independent_target_classification_respects_option_boundaries() {
        assert!(!has_independent_target(
            "display-message -F -t -t . message -t primary:0.1"
        ));
        assert!(!has_independent_target("display-message -- -t primary:0.1"));
    }

    #[test]
    fn independent_target_classification_checks_each_explicit_target_operand() {
        assert!(has_independent_target(
            "set-pane-placement -t primary:0.1 -a primary:0.0 -h tiled"
        ));
        assert!(!has_independent_target(
            "set-pane-placement -t primary:0.1 -a .0 -h tiled"
        ));
        assert!(!has_independent_target(
            "set-pane-placement -t .1 -a primary:0.0 -h tiled"
        ));
        assert!(has_independent_target(
            "set-pane-placement -t primary:0.1 floating"
        ));
    }

    #[test]
    fn independent_target_classification_accounts_for_implicit_sources() {
        for command in [
            "break-pane -t destination:2",
            "join-pane -t destination:0.0",
            "link-window -d -t destination:2",
            "move-pane -t destination:0.0",
            "move-window -t destination:2",
            "swap-pane -t destination:0.0",
            "swap-window -t destination:2",
        ] {
            assert!(!has_independent_target(command), "{command}");
        }

        for command in [
            "copy-mode -t destination:0.0",
            "link-window -d -s source:1 -t destination:2",
            "move-window -r -t destination:2",
            "set-pane-placement -t destination:0.0 floating",
            "swap-pane -D -t destination:0.0",
            "swap-pane -U -t destination:0.0",
        ] {
            assert!(has_independent_target(command), "{command}");
        }
    }

    #[test]
    fn independent_target_classification_respects_valid_marked_sources() {
        for command in [
            "join-pane -t destination:0.0",
            "move-pane -t destination:0.0",
            "swap-pane -t destination:0.0",
            "swap-window -t destination:2",
        ] {
            assert!(has_independent_target_with_mark(command), "{command}");
        }

        for command in [
            "break-pane -t destination:2",
            "link-window -d -t destination:2",
            "move-window -t destination:2",
            "swap-pane -t .0",
        ] {
            assert!(!has_independent_target_with_mark(command), "{command}");
        }
    }

    #[test]
    fn source_wrapper_defers_only_omitted_current_with_a_live_mark() {
        for command in [
            "source-file input.conf",
            "source-file -qnvF input.conf",
            "source-file -- -t",
            "source-file input.conf -t .",
            "source-file -",
        ] {
            assert!(has_independent_target_with_mark(command), "{command}");
            assert!(!has_independent_target(command), "{command}");
        }
        for command in [
            "source-file -t . input.conf",
            "source-file -qt. input.conf",
            "source-file -t primary:0.0 -t . input.conf",
            "source-file -t",
            "run-shell true",
            "if-shell -F 1 ''",
            "swap-pane -t .0",
        ] {
            assert!(!has_independent_target_with_mark(command), "{command}");
        }
        for command in [
            "source-file -t primary:0.0 input.conf",
            "source-file -qtprimary:0.0 input.conf",
            "source-file -t . -t primary:0.0 input.conf",
        ] {
            assert!(has_independent_target_with_mark(command), "{command}");
            assert!(has_independent_target(command), "{command}");
        }
    }

    #[test]
    fn independent_source_classification_uses_last_value_precedence() {
        assert!(has_independent_target(
            "copy-mode -s .1 -s primary:0.1 -t primary:0.0"
        ));
        assert!(!has_independent_target(
            "copy-mode -s primary:0.1 -s .1 -t primary:0.0"
        ));
    }

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn session_store_with_alpha_beta() -> ManagedSessionStore {
        let mut sessions = ManagedSessionStore::new();
        for name in ["alpha", "beta"] {
            sessions
                .create_session(
                    session_name(name),
                    rmux_proto::TerminalSize { cols: 80, rows: 24 },
                )
                .expect("session create succeeds");
        }
        sessions
    }

    /// One arbitrary but fixed second shared by every session in a fixture.
    const PINNED_SECOND: i64 = 1_785_500_000;

    /// Builds a store created in `m03`, `z99`, `a01` order and then used in
    /// `z99`, `m03` order, with every public second pinned to one value.
    ///
    /// `m03` is deliberately neither the alphabetically first name nor the
    /// highest creation id, so no legacy tiebreak can select it. Pinning is
    /// what makes the collision a property of the fixture instead of a race
    /// against the wall clock.
    fn same_second_session_store() -> ManagedSessionStore {
        let mut sessions = ManagedSessionStore::new();
        for name in ["m03", "z99", "a01"] {
            sessions
                .create_session(
                    session_name(name),
                    rmux_proto::TerminalSize { cols: 80, rows: 24 },
                )
                .expect("session create succeeds");
        }
        for name in ["z99", "m03"] {
            sessions
                .session_setup(&session_name(name))
                .expect("used session exists")
                .touch_attached();
        }
        for name in ["m03", "z99", "a01"] {
            sessions
                .session_setup(&session_name(name))
                .expect("created session exists")
                .pin_public_times_for_tests(PINNED_SECOND);
        }
        sessions
    }

    #[test]
    fn queue_global_fallback_selects_the_last_used_same_second_session() {
        let sessions = same_second_session_store();
        let options = OptionStore::default();

        // No requester pane, no attached session and no current target, so the
        // queue falls back to the most recently used detached session — the
        // fourth reader of the shared recency invariant.
        let context = queue_target_find_context(QueueTargetFindContextInput {
            sessions: &sessions,
            options: &options,
            requester_environment: RequesterEnvironmentContext::default(),
            attached_session: None,
            requester_owns_attached_session: false,
            current_target: None,
            current_target_follows_client: false,
            missing_current_target_fallback: MissingCurrentTargetFallback::AllowDefaultSession,
            mouse_target: None,
            marked_target: None,
        })
        .unwrap();

        assert_eq!(
            context.current(),
            Some(&Target::Pane(PaneTarget::with_window(
                session_name("m03"),
                0,
                0,
            )))
        );
    }

    #[test]
    fn queue_context_retains_inherited_linked_occurrence_and_rejects_stale_pair() {
        let mut sessions = ManagedSessionStore::new();
        let primary = session_name("queue-inherited-primary");
        let linked = session_name("queue-inherited-linked");
        let unrelated = session_name("queue-inherited-unrelated");
        for name in [&primary, &linked, &unrelated] {
            sessions
                .create_session(
                    name.clone(),
                    rmux_proto::TerminalSize { cols: 80, rows: 24 },
                )
                .expect("session create succeeds");
        }
        let (primary_id, primary_window_id, pane_id, unrelated_id) = {
            let primary_session = sessions.session(&primary).expect("primary session");
            let primary_window = primary_session.window_at(0).expect("primary window");
            let unrelated_id = sessions
                .session(&unrelated)
                .expect("unrelated session")
                .id();
            (
                primary_session.id(),
                primary_window.id(),
                primary_window.pane(0).expect("primary pane").id(),
                unrelated_id,
            )
        };
        sessions
            .link_window(
                &WindowTarget::with_window(linked.clone(), 1),
                primary_window_id,
            )
            .expect("link primary window into other session");
        let options = OptionStore::default();

        let valid = queue_target_find_context(QueueTargetFindContextInput {
            sessions: &sessions,
            options: &options,
            requester_environment: RequesterEnvironmentContext {
                pane_id: Some(pane_id.as_u32()),
                session_id: Some(primary_id),
                source_depth: None,
            },
            attached_session: None,
            requester_owns_attached_session: false,
            current_target: None,
            current_target_follows_client: false,
            missing_current_target_fallback: MissingCurrentTargetFallback::AllowDefaultSession,
            mouse_target: None,
            marked_target: None,
        })
        .expect("valid inherited occurrence resolves");
        assert_eq!(
            valid.current(),
            Some(&Target::Pane(PaneTarget::with_window(
                primary.clone(),
                0,
                0,
            )))
        );

        let stale_environment = RequesterEnvironmentContext {
            pane_id: Some(pane_id.as_u32()),
            session_id: Some(unrelated_id),
            source_depth: None,
        };
        let stale = queue_target_find_context(QueueTargetFindContextInput {
            sessions: &sessions,
            options: &options,
            requester_environment: stale_environment,
            attached_session: None,
            requester_owns_attached_session: false,
            current_target: None,
            current_target_follows_client: false,
            missing_current_target_fallback: MissingCurrentTargetFallback::AllowDefaultSession,
            mouse_target: None,
            marked_target: None,
        });
        assert!(
            matches!(stale, Err(ref error)
                if error.to_string().contains("is not present in inherited session")),
            "stale inherited pair must not fall back: {stale:?}"
        );

        let requester_attached = queue_target_find_context(QueueTargetFindContextInput {
            sessions: &sessions,
            options: &options,
            requester_environment: stale_environment,
            attached_session: Some(&primary),
            requester_owns_attached_session: true,
            current_target: None,
            current_target_follows_client: false,
            missing_current_target_fallback: MissingCurrentTargetFallback::AllowDefaultSession,
            mouse_target: None,
            marked_target: None,
        })
        .expect("requester-owned attached current precedes stale inherited identity");
        assert_eq!(
            requester_attached.current(),
            Some(&Target::Pane(PaneTarget::with_window(primary, 0, 0)))
        );

        let pinned = Target::Pane(PaneTarget::with_window(linked, 0, 0));
        let explicit = queue_target_find_context(QueueTargetFindContextInput {
            sessions: &sessions,
            options: &options,
            requester_environment: stale_environment,
            attached_session: None,
            requester_owns_attached_session: false,
            current_target: Some(&pinned),
            current_target_follows_client: false,
            missing_current_target_fallback: MissingCurrentTargetFallback::AllowDefaultSession,
            mouse_target: None,
            marked_target: None,
        })
        .expect("pinned current target ignores stale implicit environment");
        assert_eq!(explicit.current(), Some(&pinned));
    }

    #[test]
    fn queue_keeps_bare_link_window_destination_unresolved() {
        let sessions = session_store_with_alpha_beta();
        let resolved = resolve_queue_target_arguments(
            "link-window",
            vec![
                "-s".to_owned(),
                "alpha:0".to_owned(),
                "-t".to_owned(),
                "beta".to_owned(),
            ],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("alpha"),
                0,
                0,
            )))),
        )
        .expect("queue targets resolve");

        assert_eq!(resolved, ["-s", "alpha:0", "-t", "beta"]);
    }

    #[test]
    fn queue_stops_target_resolution_at_the_first_positional() {
        let sessions = session_store_with_alpha_beta();
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
            session_name("alpha"),
            0,
            0,
        ))));

        let resolved = resolve_queue_target_arguments(
            "set-option",
            ["-g", "@compact", "-tfoo"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            &sessions,
            &context,
        )
        .expect("option-like value after the option name stays positional");
        assert_eq!(resolved, ["-g", "@compact", "-tfoo"]);

        let resolved = resolve_queue_target_arguments(
            "list-windows",
            ["-F", "-tfoo", "-t", "beta"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            &sessions,
            &context,
        )
        .expect("option values are consumed before finding the positional boundary");
        assert_eq!(resolved, ["-F", "-tfoo", "-t", "beta"]);
    }

    #[test]
    fn queue_resolves_set_window_option_targets_like_set_option_window_scope() {
        let mut sessions = session_store_with_alpha_beta();
        let (window_index, _) = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created");
        sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .rename_window(window_index, "named".to_owned())
            .expect("second alpha window can be named");
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
            session_name("alpha"),
            0,
            0,
        ))));

        for (command_name, arguments, expected) in [
            (
                "set-window-option",
                vec!["-t", "+", "automatic-rename", "off"],
                vec!["-t", "alpha:1", "automatic-rename", "off"],
            ),
            (
                "set-window-option",
                vec!["-t", "named", "synchronize-panes", "on"],
                vec!["-t", "alpha:1", "synchronize-panes", "on"],
            ),
            (
                "set-option",
                vec!["-w", "-t", "+", "automatic-rename", "off"],
                vec!["-w", "-t", "alpha:1", "automatic-rename", "off"],
            ),
        ] {
            let resolved = resolve_queue_target_arguments(
                command_name,
                arguments.into_iter().map(str::to_owned).collect(),
                &sessions,
                &context,
            )
            .expect("set-window-option queue target resolves");

            assert_eq!(resolved, expected);
        }
    }

    #[test]
    fn queue_rejects_missing_set_window_option_target_like_set_option_window_scope() {
        let sessions = session_store_with_alpha_beta();
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
            session_name("alpha"),
            0,
            0,
        ))));

        let alias = resolve_queue_target_arguments(
            "set-window-option",
            ["-t", "nosuch", "automatic-rename", "off"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            &sessions,
            &context,
        )
        .expect_err("missing alias window target should fail");
        let canonical = resolve_queue_target_arguments(
            "set-option",
            ["-w", "-t", "nosuch", "automatic-rename", "off"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            &sessions,
            &context,
        )
        .expect_err("missing canonical window target should fail");

        assert_eq!(alias.to_string(), canonical.to_string());
        assert!(alias.to_string().contains("nosuch"));
    }

    #[test]
    fn queue_resolves_default_mouse_binding_compact_copy_mode_target() {
        let sessions = session_store_with_alpha_beta();
        let mouse_target = Target::Pane(PaneTarget::with_window(session_name("alpha"), 0, 0));
        let context = TargetFindContext::new(None).with_mouse_target(Some(mouse_target));

        let resolved = resolve_queue_target_arguments(
            "copy-mode",
            vec!["-Ht=".to_owned()],
            &sessions,
            &context,
        )
        .expect("copy-mode mouse target resolves");

        assert_eq!(resolved, ["-H", "-t", "alpha:0.0"]);
    }

    #[test]
    fn queue_resolves_default_mouse_binding_compact_send_keys_target() {
        let sessions = session_store_with_alpha_beta();
        let mouse_target = Target::Pane(PaneTarget::with_window(session_name("alpha"), 0, 0));
        let context = TargetFindContext::new(None).with_mouse_target(Some(mouse_target));

        let resolved = resolve_queue_target_arguments(
            "send-keys",
            vec!["-Xt=".to_owned(), "select-word".to_owned()],
            &sessions,
            &context,
        )
        .expect("send-keys mouse target resolves");

        assert_eq!(resolved, ["-X", "-t", "alpha:0.0", "select-word"]);
    }

    #[test]
    fn typed_queue_target_resolution_canonicalizes_run_shell_pane_id() {
        let sessions = session_store_with_alpha_beta();
        let pane_id = sessions
            .session(&session_name("alpha"))
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id().to_string())
            .expect("alpha pane id exists");
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
            session_name("beta"),
            0,
            0,
        ))));

        let resolved =
            resolve_queue_target_argument_typed("run-shell", 't', pane_id, &sessions, &context)
                .expect("run-shell pane id resolves");

        assert_eq!(
            resolved,
            QueueTargetArgumentResolution::Resolved("alpha:0.0".to_owned())
        );
    }

    #[test]
    fn typed_queue_target_resolution_reports_run_shell_canfail_target() {
        let sessions = session_store_with_alpha_beta();
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
            session_name("alpha"),
            0,
            0,
        ))));

        let resolved = resolve_queue_target_argument_typed(
            "run-shell",
            't',
            "%9999".to_owned(),
            &sessions,
            &context,
        )
        .expect("run-shell missing pane id is CANFAIL");

        assert_eq!(resolved, QueueTargetArgumentResolution::CanFail);
    }

    #[test]
    fn queue_resolves_compact_swap_window_source_and_target_values() {
        let mut sessions = session_store_with_alpha_beta();
        let (window_index, _) = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created");
        sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .rename_window(window_index, "one".to_owned())
            .expect("second alpha window can be named");
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
            session_name("alpha"),
            0,
            0,
        ))));

        for (arguments, expected) in [
            (
                vec!["-ds", "one", "-t", "alpha:0"],
                vec!["-d", "-s", "alpha:1", "-t", "alpha:0"],
            ),
            (
                vec!["-dt", "+1", "-s", "alpha:0"],
                vec!["-d", "-t", "alpha:1", "-s", "alpha:0"],
            ),
        ] {
            let resolved = resolve_queue_target_arguments(
                "swap-window",
                arguments.into_iter().map(str::to_owned).collect(),
                &sessions,
                &context,
            )
            .expect("compact swap-window targets resolve");

            assert_eq!(resolved, expected);
        }
    }

    #[test]
    fn queue_resolves_status_mouse_switch_client_target_as_pane() {
        let mut sessions = session_store_with_alpha_beta();
        let (window_index, _) = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created");
        let mouse_target = Target::Window(WindowTarget::with_window(
            session_name("alpha"),
            window_index,
        ));
        let context = TargetFindContext::new(None).with_mouse_target(Some(mouse_target));

        let resolved = resolve_queue_target_arguments(
            "switch-client",
            vec!["-t=".to_owned()],
            &sessions,
            &context,
        )
        .expect("switch-client mouse target resolves");

        assert_eq!(resolved, ["-t", "alpha:1.0"]);
    }

    #[test]
    fn queue_resolves_switch_client_bare_target_as_session() {
        let mut sessions = session_store_with_alpha_beta();
        sessions
            .create_session(
                session_name("2"),
                rmux_proto::TerminalSize { cols: 80, rows: 24 },
            )
            .expect("numeric session create succeeds");
        let mut alpha = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists");
        alpha
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created");
        alpha
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("third alpha window can be created");

        let resolved = resolve_queue_target_arguments(
            "switch-client",
            vec!["-t".to_owned(), "2".to_owned()],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("alpha"),
                0,
                0,
            )))),
        )
        .expect("switch-client bare target resolves");

        assert_eq!(resolved, ["-t", "2"]);
    }

    #[test]
    fn queue_resolves_switch_client_bare_target_as_session_when_window_name_collides() {
        let mut sessions = session_store_with_alpha_beta();
        sessions
            .create_session(
                session_name("editor"),
                rmux_proto::TerminalSize { cols: 80, rows: 24 },
            )
            .expect("editor session create succeeds");
        let editor_window = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created")
            .0;
        sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .rename_window(editor_window, "editor".to_owned())
            .expect("alpha window rename succeeds");

        let resolved = resolve_queue_target_arguments(
            "switch-client",
            vec!["-t".to_owned(), "editor".to_owned()],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("alpha"),
                0,
                0,
            )))),
        )
        .expect("switch-client bare collision target resolves");

        assert_eq!(resolved, ["-t", "editor"]);
        assert_ne!(editor_window, 0);
    }

    #[test]
    fn queue_rejects_switch_client_bare_numeric_window_without_session_match() {
        let mut sessions = session_store_with_alpha_beta();
        let mut alpha = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists");
        alpha
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created");
        alpha
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("third alpha window can be created");

        let error = resolve_queue_target_arguments(
            "switch-client",
            vec!["-t".to_owned(), "2".to_owned()],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("alpha"),
                0,
                0,
            )))),
        )
        .expect_err("switch-client bare numeric window index requires a session match");

        assert!(error.to_string().contains("can't find session: 2"));
    }

    #[test]
    fn queue_resolves_switch_client_window_id_target_as_window() {
        let mut sessions = session_store_with_alpha_beta();
        let (window_index, _) = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists")
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created");
        let window_id = sessions
            .session(&session_name("alpha"))
            .and_then(|session| session.window_at(window_index))
            .map(|window| window.id().to_string())
            .expect("second alpha window id exists");

        let resolved = resolve_queue_target_arguments(
            "switch-client",
            vec!["-t".to_owned(), window_id],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("beta"),
                0,
                0,
            )))),
        )
        .expect("switch-client window id target resolves");

        assert_eq!(
            resolved,
            vec!["-t".to_owned(), format!("alpha:{window_index}")]
        );
    }

    #[test]
    fn queue_preserves_switch_client_equals_prefixed_session_target() {
        let mut sessions = session_store_with_alpha_beta();
        sessions
            .create_session(
                session_name("2"),
                rmux_proto::TerminalSize { cols: 80, rows: 24 },
            )
            .expect("equals-prefixed session create succeeds");
        let mut alpha = sessions
            .session_setup(&session_name("alpha"))
            .expect("alpha session exists");
        alpha
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("second alpha window can be created");
        alpha
            .create_window(rmux_proto::TerminalSize { cols: 80, rows: 24 })
            .expect("third alpha window can be created");

        let resolved = resolve_queue_target_arguments(
            "switch-client",
            vec!["-t".to_owned(), "=2".to_owned()],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("alpha"),
                0,
                0,
            )))),
        )
        .expect("switch-client equals-prefixed session target resolves");

        assert_eq!(resolved, ["-t", "2"]);
    }

    #[test]
    fn queue_preserves_switch_client_pane_qualified_target() {
        let sessions = session_store_with_alpha_beta();

        let resolved = resolve_queue_target_arguments(
            "switch-client",
            vec!["-t".to_owned(), "beta:0.0".to_owned()],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("alpha"),
                0,
                0,
            )))),
        )
        .expect("switch-client pane target resolves");

        assert_eq!(resolved, ["-t", "beta:0.0"]);
    }

    #[test]
    fn queue_preserves_attach_session_explicit_target_kind() {
        let sessions = session_store_with_alpha_beta();
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::new(
            session_name("alpha"),
            0,
        ))));
        for (raw, expected) in [
            ("beta", "beta"),
            ("beta:0", "beta:0.0"),
            ("beta:0.0", "beta:0.0"),
        ] {
            for arguments in [
                vec!["-t".to_owned(), raw.to_owned()],
                vec![format!("-t{raw}")],
            ] {
                let resolved = resolve_queue_target_arguments(
                    "attach-session",
                    arguments,
                    &sessions,
                    &context,
                )
                .unwrap();
                assert_eq!(resolved.last().unwrap(), expected, "{raw}");
            }
        }
    }

    #[test]
    fn queue_resolves_explicit_default_marked_source_without_falling_back_to_marked() {
        let sessions = session_store_with_alpha_beta();
        let marked_target = Target::Pane(PaneTarget::with_window(session_name("beta"), 0, 0));
        let context = TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
            session_name("beta"),
            0,
            0,
        ))))
        .with_marked_target(Some(marked_target));

        let resolved = resolve_queue_target_arguments(
            "join-pane",
            vec![
                "-s".to_owned(),
                "alpha:0.0".to_owned(),
                "-t".to_owned(),
                "beta:0.0".to_owned(),
            ],
            &sessions,
            &context,
        )
        .expect("queue targets resolve");

        assert_eq!(resolved, ["-s", "alpha:0.0", "-t", "beta:0.0"]);
    }
}
