use super::*;

fn scope(owner: u64) -> StatusJobScope {
    StatusJobScope {
        owner: StatusJobOwner::Subscription(owner),
        session: Some(1),
        window: Some(2),
        window_index: Some(0),
        pane: Some(3),
        client: None,
    }
}

fn key(command: &str, owner: u64) -> StatusJobKey {
    let mut key = StatusJobKey::new(command, None);
    key.source = Some("raw #{variable}".into());
    key.scope = Some(scope(owner));
    key
}

fn entry(output: &str, id: u64) -> StatusJobCacheEntry {
    StatusJobCacheEntry {
        output: output.into(),
        updated_at: Some(Instant::now()),
        in_flight: true,
        job_id: Some(id),
    }
}

fn fill_active(state: &mut StatusJobRuntimeState) {
    for id in 0..STATUS_JOB_ACTIVE_LIMIT as u64 {
        state.active.insert(
            id,
            ActiveStatusJob {
                cancellation: Arc::new(AtomicBool::new(false)),
                worker: None,
                completed: false,
            },
        );
    }
}

#[test]
fn changed_input_revokes_publication_even_when_all_worker_slots_are_busy() {
    let runtime = StatusJobRuntime::new();
    let old = key("old", 1);
    {
        let mut state = runtime.inner.lock_state();
        fill_active(&mut state);
        state.cache.insert(old.clone(), entry("previous", 0));
    }
    assert_eq!(
        runtime.scoped_output(
            "raw #{variable}",
            "new",
            None,
            scope(1),
            Duration::from_secs(60)
        ),
        "previous"
    );
    runtime.inner.publish_job(0, &old, "obsolete-stream".into());
    runtime.inner.complete_job(0, &old, "obsolete-final".into());
    let state = runtime.inner.lock_state();
    assert!(state.active[&0].cancellation.load(Ordering::Acquire));
    assert!(!state.cache.contains_key(&old));
    assert_eq!(state.cache[&key("new", 1)].output, "previous");
}

#[test]
fn same_key_recreated_after_invalidation_rejects_the_old_worker_generation() {
    let runtime = StatusJobRuntime::new();
    let old = key("old", 1);
    runtime.seed_cache(old.clone(), entry("new-generation", 9));
    runtime.inner.publish_job(8, &old, "stale".into());
    runtime.inner.complete_job(8, &old, "stale".into());
    let state = runtime.inner.lock_state();
    assert_eq!(state.cache[&old].output, "new-generation");
    assert!(state.cache[&old].in_flight);
    assert_eq!(state.cache[&old].job_id, Some(9));
}

#[test]
fn owner_and_canonical_context_are_distinct_cache_dimensions() {
    let first = key("same", 1);
    let second = key("same", 2);
    let mut other_pane = first.clone();
    other_pane.scope.as_mut().unwrap().pane = Some(4);
    let mut state = StatusJobRuntimeState::default();
    state.cache.insert(first.clone(), entry("one", 1));
    state.cache.insert(second.clone(), entry("two", 2));
    state.cache.insert(other_pane.clone(), entry("pane", 3));
    assert_eq!(
        invalidate_changed_input(&mut state, &key("changed", 1)),
        Some("one".into())
    );
    assert_eq!(state.cache[&second].output, "two");
    assert_eq!(state.cache[&other_pane].output, "pane");
}

#[test]
fn linked_window_occurrences_do_not_invalidate_each_other() {
    let first = key("index-zero", 1);
    let mut linked = key("index-two", 1);
    linked.scope.as_mut().unwrap().window_index = Some(2);
    let mut state = StatusJobRuntimeState::default();
    state.cache.insert(first.clone(), entry("zero", 1));

    assert_eq!(invalidate_changed_input(&mut state, &linked), None);
    assert_eq!(state.cache[&first].output, "zero");
}

#[test]
fn profile_invalidation_discards_previous_output_and_cancels_its_worker() {
    let old = key("same", 1);
    let mut changed = old.clone();
    changed.cwd = Some("different-directory".into());
    let mut state = StatusJobRuntimeState::default();
    fill_active(&mut state);
    state
        .cache
        .insert(old.clone(), entry("private-to-old-directory", 0));
    assert_eq!(invalidate_changed_input(&mut state, &changed), None);
    assert!(!state.cache.contains_key(&old));
    assert!(state.active[&0].cancellation.load(Ordering::Acquire));
}

#[test]
fn initial_subscription_rechecks_retained_output_and_completion_releases_capacity() {
    let runtime = StatusJobRuntime::new();
    let key = key("same", 1);
    runtime.seed_cache(key.clone(), entry("same", 0));
    let mut updates = runtime.subscribe();
    assert!(updates.has_changed().unwrap());
    updates.borrow_and_update();
    runtime.inner.complete_job(0, &key, "same".into());
    assert!(
        updates.has_changed().unwrap(),
        "unchanged completion still retries denied consumers"
    );
}

#[test]
fn obsolete_completion_is_quiet_without_capacity_pressure() {
    let runtime = StatusJobRuntime::new();
    let current = key("current", 1);
    runtime.seed_cache(current, entry("current", 9));
    let mut updates = runtime.subscribe();
    updates.borrow_and_update();

    runtime
        .inner
        .complete_job(8, &key("obsolete", 1), String::new());

    assert!(!updates.has_changed().unwrap());
}

#[test]
fn one_obsolete_completion_wakes_capacity_waiters() {
    let runtime = StatusJobRuntime::new();
    {
        let mut state = runtime.inner.lock_state();
        fill_active(&mut state);
    }
    assert_eq!(
        runtime.scoped_output(
            "capacity source",
            "capacity command",
            None,
            scope(2),
            Duration::from_secs(60),
        ),
        ""
    );
    let mut updates = runtime.subscribe();
    updates.borrow_and_update();

    runtime
        .inner
        .complete_job(0, &key("obsolete", 1), String::new());
    assert!(updates.has_changed().unwrap());
    updates.borrow_and_update();
    runtime
        .inner
        .complete_job(1, &key("also obsolete", 1), String::new());
    assert!(!updates.has_changed().unwrap());
}

#[cfg(unix)]
#[test]
fn last_line_and_final_fragment_are_bounded_job_results() {
    for (command, expected) in [
        ("printf 'first\\nlast\\n'", "last"),
        ("printf 'first\\n\\n'", ""),
        ("printf 'first\\nfragment'", "fragment"),
        ("printf 'line\\r\\n'", "line"),
    ] {
        assert_eq!(run_status_job(command, None), expected);
    }
}

#[cfg(unix)]
#[test]
fn streaming_result_is_available_before_process_completion() {
    let runtime = StatusJobRuntime::new();
    let command = "printf 'intermediate\\n'; sleep .4; printf final";
    assert_eq!(
        runtime.cached_output(command, None, Duration::from_secs(60)),
        ""
    );
    let deadline = Instant::now() + Duration::from_millis(300);
    loop {
        if runtime.cached_output(command, None, Duration::from_secs(60)) == "intermediate" {
            assert_eq!(runtime.active_job_count(), 1);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "line not published while the child was alive"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime.cached_output(command, None, Duration::from_secs(60)) != "final" {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_empty_runtime_does_not_schedule_an_extra_attach_frame() {
    let runtime = StatusJobRuntime::new();
    assert!(!runtime.subscribe().has_changed().unwrap());
}

#[test]
fn command_owners_include_the_registration_kind_not_only_pid_and_counter() {
    let mut attached = key("same", 1);
    attached.scope.as_mut().unwrap().owner = StatusJobOwner::Command {
        control: false,
        pid: 7,
        generation: 1,
    };
    let mut control = attached.clone();
    control.scope.as_mut().unwrap().owner = StatusJobOwner::Command {
        control: true,
        pid: 7,
        generation: 1,
    };
    let mut state = StatusJobRuntimeState::default();
    state.cache.insert(attached.clone(), entry("attached", 1));
    state.cache.insert(control.clone(), entry("control", 2));
    let mut changed = attached.clone();
    changed.command = "changed".into();
    assert_eq!(
        invalidate_changed_input(&mut state, &changed),
        Some("attached".into())
    );
    assert_eq!(state.cache[&control].output, "control");
    assert!(!state.cache.contains_key(&attached));
}

#[test]
fn explicit_refresh_keeps_the_last_line_but_revokes_only_its_owners_worker() {
    let runtime = StatusJobRuntime::new();
    let first = key("same", 1);
    let peer = key("same", 2);
    {
        let mut state = runtime.inner.lock_state();
        fill_active(&mut state);
        state.cache.insert(first.clone(), entry("previous", 0));
        state.cache.insert(peer.clone(), entry("peer", 1));
    }
    runtime.refresh_owner(&scope(1).owner);
    runtime.inner.publish_job(0, &first, "obsolete".into());
    runtime.inner.complete_job(0, &first, "obsolete".into());
    let state = runtime.inner.lock_state();
    assert_eq!(state.cache[&first].output, "previous");
    assert!(state.cache[&first].updated_at.is_none());
    assert!(!state.cache[&first].in_flight);
    assert!(state.active[&0].cancellation.load(Ordering::Acquire));
    assert_eq!(state.cache[&peer].output, "peer");
    assert!(state.cache[&peer].in_flight);
    assert!(!state.active[&1].cancellation.load(Ordering::Acquire));
}
