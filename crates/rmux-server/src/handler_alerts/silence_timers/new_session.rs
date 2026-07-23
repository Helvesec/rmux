use rmux_proto::{SessionId, SessionName, WindowTarget};

use super::{desired_silence_timers, monitor_silence_seconds, RequestHandler, SilenceTimerState};

impl RequestHandler {
    pub(in crate::handler) async fn sync_new_session_silence_timers(
        &self,
        session_name: &SessionName,
        session_id: SessionId,
        template_session: Option<&SessionName>,
    ) {
        // Serialize with cancel_session_silence_timers: every state-derived
        // timer producer holds the state lock across its timers-map mutation,
        // or a concurrent kill-session can cancel between our state read and
        // our insert and the dead session resurrects an orphan timer.
        let state = self.state.lock().await;
        let (desired, inherited_from) = {
            let Some(session) = state
                .sessions
                .session(session_name)
                .filter(|session| session.id() == session_id)
            else {
                return;
            };
            let targets = session
                .windows()
                .keys()
                .copied()
                .map(|window_index| WindowTarget::with_window(session_name.clone(), window_index))
                .collect::<Vec<_>>();
            let desired = desired_silence_timers(&state, &targets);
            let group_members = state.sessions.session_group_members(session_name);
            let mut inheritance_sources = Vec::new();
            if let Some(template_session) = template_session.filter(|template| {
                *template != session_name && state.sessions.session(template).is_some()
            }) {
                inheritance_sources.push(template_session.clone());
            }
            for member in group_members {
                if member != *session_name && !inheritance_sources.contains(&member) {
                    inheritance_sources.push(member);
                }
            }
            let inherited_from = desired
                .iter()
                .map(|desired| {
                    inheritance_sources.iter().find_map(|member| {
                        let member_session = state.sessions.session(member)?;
                        let preferred_index = desired.target.window_index();
                        let source_target = if member_session
                            .window_at(preferred_index)
                            .is_some_and(|window| window.id() == desired.window_id)
                        {
                            WindowTarget::with_window(member.clone(), preferred_index)
                        } else {
                            let mut matches = member_session
                                .windows()
                                .iter()
                                .filter(|(_, window)| window.id() == desired.window_id)
                                .map(|(&index, _)| {
                                    WindowTarget::with_window(member.clone(), index)
                                });
                            let only_match = matches.next()?;
                            matches.next().is_none().then_some(only_match)?
                        };
                        let was_monitored = monitor_silence_seconds(
                            &state.options,
                            source_target.session_name(),
                            source_target.window_index(),
                        ) > 0;
                        Some((source_target, was_monitored))
                    })
                })
                .collect::<Vec<_>>();
            (desired, inherited_from)
        };

        let admission_count = desired
            .iter()
            .filter(|desired| desired.seconds != 0)
            .count();
        let Some(mut timer_reservations) = self.reserve_silence_timer_tasks(admission_count) else {
            return;
        };
        let mut timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        for (desired, inheritance) in desired.into_iter().zip(inherited_from) {
            let next_generation = timers
                .get(&desired.target)
                .map_or(1, |timer| timer.generation.saturating_add(1));
            if let Some(previous) = timers.remove(&desired.target) {
                previous.task.abort();
            }
            if desired.seconds == 0 {
                continue;
            }
            let (generation, deadline) = match inheritance {
                Some((source_target, true)) => {
                    let inherited_deadline = timers.get(&source_target).and_then(|source| {
                        (source.window_id == desired.window_id).then_some(source.deadline)
                    });
                    let Some(deadline) = inherited_deadline else {
                        // The matching family timer already expired. A new grouped
                        // alias inherits that expired state and must not rearm it.
                        continue;
                    };
                    (next_generation, deadline)
                }
                // `configure_silence_timer` previously ran after the stale target entry was
                // removed, so fresh targets restart at generation one.
                Some((_, false)) | None => (
                    1,
                    tokio::time::Instant::now() + std::time::Duration::from_secs(desired.seconds),
                ),
            };
            let task = self.spawn_silence_timer_task(
                desired.target.clone(),
                desired.session_id,
                desired.window_id,
                generation,
                deadline,
                timer_reservations.take(),
            );
            timers.insert(
                desired.target,
                SilenceTimerState {
                    session_id: desired.session_id,
                    window_id: desired.window_id,
                    generation,
                    deadline,
                    task,
                },
            );
        }
        drop(timers);
        drop(timer_reservations);
        drop(state);
    }
}
