use super::*;

pub(super) fn hook_only_format_values(hook: HookName) -> Vec<(String, String)> {
    vec![("hook".to_owned(), hook.to_string())]
}

pub(in crate::handler) fn after_hook_format_values(
    hook: HookName,
    parsed_command: Option<&ParsedCommand>,
) -> Vec<(String, String)> {
    let mut formats = hook_only_format_values(hook);
    let Some(parsed_command) = parsed_command
        .filter(|_| super::super::scripting_support::queued_command_exposes_hook_arguments())
    else {
        return formats;
    };

    let arguments = parsed_command
        .arguments()
        .iter()
        .map(CommandArgument::to_tmux_string)
        .collect::<Vec<_>>();
    formats.push(("hook_arguments".to_owned(), arguments.join(" ")));
    for (index, argument) in arguments.iter().enumerate() {
        formats.push((format!("hook_argument_{index}"), argument.clone()));
    }

    let scalar_arguments = parsed_command
        .arguments()
        .iter()
        .filter_map(CommandArgument::as_string)
        .collect::<Vec<_>>();
    let mut flag_values = BTreeMap::<char, Vec<String>>::new();
    let mut index = 0;
    while index < scalar_arguments.len() {
        let token = scalar_arguments[index];
        if token == "--" {
            break;
        }
        let Some(flags) = token.strip_prefix('-') else {
            index += 1;
            continue;
        };
        if flags.is_empty()
            || token.starts_with("--")
            || !flags.chars().all(|flag| flag.is_ascii_alphabetic())
        {
            index += 1;
            continue;
        }

        if flags.len() == 1 {
            let flag = flags.chars().next().expect("single-char flag");
            if let Some(value) = scalar_arguments.get(index + 1).copied() {
                if !value.starts_with('-') {
                    flag_values.entry(flag).or_default().push(value.to_owned());
                    index += 2;
                    continue;
                }
            }
        }

        for flag in flags.chars() {
            let _ = flag_values.entry(flag).or_default();
        }
        index += 1;
    }

    for (flag, values) in flag_values {
        if let Some(value) = values.last() {
            formats.push((format!("hook_flag_{flag}"), value.clone()));
            for (index, value) in values.into_iter().enumerate() {
                formats.push((format!("hook_flag_{flag}_{index}"), value));
            }
        } else {
            formats.push((format!("hook_flag_{flag}"), "1".to_owned()));
        }
    }

    formats
}

pub(super) fn lifecycle_hook_formats(
    state: &crate::pane_terminals::HandlerState,
    event: &LifecycleEvent,
) -> Vec<(String, String)> {
    let mut formats = hook_only_format_values(event.hook_name());
    if let Some(client_name) = event.client_name() {
        formats.push(("hook_client".to_owned(), client_name.to_owned()));
    }
    if let Some(session_name) = event.session_name() {
        if let Some(session) = state.sessions.session(session_name) {
            formats.push(("hook_session".to_owned(), session.id().to_string()));
            formats.push(("hook_session_name".to_owned(), session.name().to_string()));
        } else {
            if let Some(session_id) = event.session_id() {
                formats.push(("hook_session".to_owned(), format!("${session_id}")));
            }
            formats.push(("hook_session_name".to_owned(), session_name.to_string()));
        }
    }
    if let LifecycleEvent::WindowUnlinked {
        window_id,
        window_name,
        ..
    } = event
    {
        if let Some(window_id) = window_id {
            formats.push(("hook_window".to_owned(), format!("@{window_id}")));
        }
        if let Some(window_name) = window_name {
            formats.push(("hook_window_name".to_owned(), window_name.clone()));
        }
    } else if let Some(window_target) = event.window_target() {
        let mut resolved_window = false;
        if let Some(session) = state.sessions.session(window_target.session_name()) {
            if let Some(window) = session.window_at(window_target.window_index()) {
                formats.push(("hook_window".to_owned(), window.id().to_string()));
                formats.push((
                    "hook_window_name".to_owned(),
                    window.name().unwrap_or_default().to_owned(),
                ));
                resolved_window = true;
            }
        }
        if !resolved_window {
            if let Some(window_id) = event.window_id() {
                formats.push(("hook_window".to_owned(), format!("@{window_id}")));
                if let Some(window_name) = event.window_name_snapshot() {
                    formats.push(("hook_window_name".to_owned(), window_name.to_owned()));
                }
            }
        }
    }
    if let Some(pane_target) = event.pane_target() {
        let mut resolved_pane = false;
        if let Some(session) = state.sessions.session(pane_target.session_name()) {
            if let Some(window) = session.window_at(pane_target.window_index()) {
                if let Some(pane) = window.pane(pane_target.pane_index()) {
                    formats.push(("hook_pane".to_owned(), format!("%{}", pane.id().as_u32())));
                    resolved_pane = true;
                }
            }
        }
        if !resolved_pane {
            if let Some(pane_id) = event.pane_id() {
                formats.push(("hook_pane".to_owned(), format!("%{pane_id}")));
            }
        }
        if matches!(event, LifecycleEvent::PaneFocusOut { .. }) {
            // A focused runtime can disappear before its PO is prepared. Keep
            // the full pane identity in the deferred hook formats while the
            // runtime is still live, independently of any later slot reuse.
            append_pane_format_values(state, pane_target, &mut formats);
        }
    }
    if let LifecycleEvent::PaneModeChanged { target } = event {
        if let Ok(transcript) = state.transcript_handle(target) {
            let transcript = transcript.lock();
            formats.push((
                "pane_in_mode".to_owned(),
                u8::from(transcript.pane_in_mode()).to_string(),
            ));
            formats.push((
                "pane_mode".to_owned(),
                transcript.pane_mode_name().unwrap_or_default().to_owned(),
            ));
        }
    }
    if matches!(
        event,
        LifecycleEvent::PaneExited { .. } | LifecycleEvent::PaneDied { .. }
    ) {
        match lifecycle_hook_current_target(state, event) {
            Some(Target::Pane(target)) => append_pane_format_values(state, &target, &mut formats),
            _ => {
                if let Some(session_name) = event.session_name() {
                    if let Some(Target::Pane(target)) =
                        first_session_pane_target(state, session_name)
                    {
                        append_pane_format_values(state, &target, &mut formats);
                    }
                }
            }
        }
    }
    formats
}

pub(super) fn append_pane_format_values(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
    formats: &mut Vec<(String, String)>,
) {
    let Some(session) = state.sessions.session(target.session_name()) else {
        return;
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return;
    };
    let Some(pane) = window.pane(target.pane_index()) else {
        return;
    };
    formats.push(("window_index".to_owned(), target.window_index().to_string()));
    formats.push(("pane_index".to_owned(), target.pane_index().to_string()));
    formats.push(("pane_id".to_owned(), pane.id().to_string()));
}
