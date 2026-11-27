use super::*;

#[derive(Debug)]
pub(super) enum PaneTranscriptTimer {
    Ground(PaneGroundTimer),
    SynchronizedOutput(PaneSynchronizedOutputTimer),
}

impl PaneTranscriptTimer {
    pub(super) const fn deadline(&self) -> Instant {
        match self {
            Self::Ground(timer) => timer.deadline,
            Self::SynchronizedOutput(timer) => timer.deadline,
        }
    }
}

pub(super) struct PaneTranscriptTimerJob {
    pub(super) transcript: SharedPaneTranscript,
    pub(super) pane_output: PaneOutputSender,
    pub(super) timer: PaneTranscriptTimer,
}

pub(super) fn schedule_pane_transcript_timer(
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    timer: PaneTranscriptTimer,
) {
    schedule_pane_transcript_timer_with_sender(
        pane_transcript_timer_tx(),
        session_name,
        pane_id,
        transcript,
        pane_output,
        timer,
    );
}

pub(super) fn schedule_pane_transcript_timer_with_sender(
    timer_tx: &std_mpsc::Sender<PaneTranscriptTimerJob>,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    timer: PaneTranscriptTimer,
) {
    // The timer owns the synchronized-output claim until `send` accepts the
    // job. `SendError` returns that whole job below, whose drop rolls the claim
    // back so a later append can re-arm rather than remaining latched.
    let job = PaneTranscriptTimerJob {
        transcript,
        pane_output,
        timer,
    };
    if let Err(error) = timer_tx.send(job) {
        warn!(
            session = %session_name,
            pane_id = pane_id.as_u32(),
            "failed to schedule pane transcript timer: {error}"
        );
        drop(error.0);
    }
}

pub(super) fn pane_transcript_timer_tx() -> &'static std_mpsc::Sender<PaneTranscriptTimerJob> {
    static TIMER_TX: OnceLock<std_mpsc::Sender<PaneTranscriptTimerJob>> = OnceLock::new();
    TIMER_TX.get_or_init(|| {
        let (tx, rx) = std_mpsc::channel();
        spawn_pane_transcript_timer_worker(rx);
        tx
    })
}

pub(super) fn spawn_pane_transcript_timer_worker(rx: std_mpsc::Receiver<PaneTranscriptTimerJob>) {
    let thread_name = "rmux-pane-transcript-timer".to_owned();
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || run_pane_transcript_timer_worker(rx))
    {
        warn!(
            thread = %thread_name,
            "failed to spawn pane transcript timer worker: {error}"
        );
    }
}

pub(super) fn run_pane_transcript_timer_worker(rx: std_mpsc::Receiver<PaneTranscriptTimerJob>) {
    let mut jobs = Vec::<PaneTranscriptTimerJob>::new();
    loop {
        if jobs.is_empty() {
            match rx.recv() {
                Ok(job) => {
                    jobs.push(job);
                    continue;
                }
                Err(_) => return,
            }
        }

        expire_due_pane_transcript_timers(&mut jobs);
        if jobs.is_empty() {
            continue;
        }

        let next_deadline = jobs
            .iter()
            .map(|job| job.timer.deadline())
            .min()
            .expect("non-empty timer job queue has a deadline");
        let timeout = next_deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(job) => jobs.push(job),
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                expire_due_pane_transcript_timers(&mut jobs)
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub(super) fn expire_due_pane_transcript_timers(jobs: &mut Vec<PaneTranscriptTimerJob>) {
    let now = Instant::now();
    let mut index = 0;
    while index < jobs.len() {
        if now < jobs[index].timer.deadline() {
            index += 1;
            continue;
        }
        let job = jobs.swap_remove(index);
        // `job` moves into the unwind boundary. A panic cannot strand its
        // synchronized-output claim: unwinding drops the job (and timer),
        // while a stale job explicitly moves that ownership into `replacement`.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            expire_pane_transcript_timer_job(job)
        })) {
            Ok(Some(replacement)) => jobs.push(replacement),
            Ok(None) => {}
            Err(_) => {
                warn!("pane transcript timer job panicked; timer worker is continuing");
            }
        }
    }
}

pub(super) fn expire_pane_transcript_timer_job(
    job: PaneTranscriptTimerJob,
) -> Option<PaneTranscriptTimerJob> {
    let PaneTranscriptTimerJob {
        transcript,
        pane_output,
        timer,
    } = job;
    let replacement = pane_output.mutate_transcript(
        &transcript,
        crate::pane_io::PaneInvalidationReason::ParserStateExpired,
        |transcript| {
            let (expired, replacement) = match timer {
                PaneTranscriptTimer::Ground(timer) => (transcript.expire_ground_timer(timer), None),
                PaneTranscriptTimer::SynchronizedOutput(timer) => {
                    transcript.handle_synchronized_output_timer(timer)
                }
            };
            (replacement, expired)
        },
    );
    replacement.map(|timer| PaneTranscriptTimerJob {
        transcript,
        pane_output,
        timer: PaneTranscriptTimer::SynchronizedOutput(timer),
    })
}

pub(super) fn publish_pane_eof(
    transcript: &SharedPaneTranscript,
    pane_output: &PaneOutputSender,
    generation: Option<u64>,
) {
    let synchronized_output_active =
        transcript.lock_for_eof().mode() & rmux_core::input::mode::MODE_SYNC != 0;
    if !synchronized_output_active {
        // Expire OSC 7501 active reports under the generation guard, so a
        // stale reader's EOF cannot touch a respawned pane.
        let _ =
            pane_output.publish_for_generation_with_invalidation(generation, Vec::new(), |_| {
                transcript.lock_for_eof().expire_program_status();
                ((), Vec::new(), None)
            });
        return;
    }

    // Release the converged screen under the same generation-guarded output
    // boundary that publishes EOF. A stale reader therefore cannot release a
    // replacement process, and final attach drains render before closing. A
    // queued timer keeps its own claim until it is later dropped; generation
    // replacement uses a distinct transcript and therefore cannot inherit it.
    let _ = pane_output.publish_for_generation_with_invalidation(generation, Vec::new(), |_| {
        let mut transcript = transcript.lock_for_eof();
        transcript.expire_program_status();
        let invalidation = transcript
            .finish_synchronized_output()
            .then_some(crate::pane_io::PaneInvalidationReason::TranscriptMutation);
        ((), Vec::new(), invalidation)
    });
}
