use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use super::*;

fn close_normal_gate_without_notifying_watch(registry: &LifecycleProducerRegistry) {
    registry
        .state
        .lock()
        .expect("lifecycle producer registry must not be poisoned")
        .accepting_normal = false;
}

#[tokio::test]
async fn close_cancels_a_pending_task_before_mutation() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let started = Arc::new(Notify::new());
    let trigger = Arc::new(Notify::new());
    let mutated = Arc::new(AtomicBool::new(false));
    let task = {
        let started = Arc::clone(&started);
        let trigger = Arc::clone(&trigger);
        let mutated = Arc::clone(&mutated);
        tokio::spawn(run_registered_lifecycle_producer(
            registration,
            async move {
                started.notify_one();
                trigger.notified().await;
                let Some(_mutation) = begin_current_lifecycle_mutation() else {
                    return;
                };
                mutated.store(true, Ordering::SeqCst);
            },
        ))
    };

    started.notified().await;
    registry.close_and_wait().await;
    trigger.notify_waiters();

    assert_eq!(task.await.expect("pending producer joins"), None);
    assert!(!mutated.load(Ordering::SeqCst));
    assert!(registry.try_register().is_none());
}

#[tokio::test]
async fn close_waits_for_mutation_and_publication() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let mutation_reached = Arc::new(Notify::new());
    let release_publication = Arc::new(Notify::new());
    let published = Arc::new(AtomicBool::new(false));
    let task = {
        let mutation_reached = Arc::clone(&mutation_reached);
        let release_publication = Arc::clone(&release_publication);
        let published = Arc::clone(&published);
        tokio::spawn(run_registered_lifecycle_producer(
            registration,
            async move {
                let _mutation = begin_current_lifecycle_mutation().expect("mutation admitted");
                mutation_reached.notify_one();
                release_publication.notified().await;
                published.store(true, Ordering::SeqCst);
            },
        ))
    };

    mutation_reached.notified().await;
    let close = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_and_wait().await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!close.is_finished(), "shutdown must wait for publication");

    release_publication.notify_one();
    close.await.expect("shutdown drain joins");
    assert_eq!(task.await.expect("mutating producer joins"), Some(()));
    assert!(published.load(Ordering::SeqCst));
}

#[tokio::test]
async fn close_cancels_follow_on_pending_work_after_a_mutation() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let published = Arc::new(Notify::new());
    let never = Arc::new(Notify::new());
    let task = {
        let published = Arc::clone(&published);
        let never = Arc::clone(&never);
        tokio::spawn(run_registered_lifecycle_producer(
            registration,
            async move {
                {
                    let _mutation = begin_current_lifecycle_mutation().expect("mutation admitted");
                    published.notify_one();
                }
                never.notified().await;
            },
        ))
    };

    published.notified().await;
    tokio::time::timeout(Duration::from_millis(100), registry.close_and_wait())
        .await
        .expect("shutdown cancels follow-on pending work");
    assert_eq!(task.await.expect("producer joins"), None);
}

#[tokio::test]
async fn cancellation_cleanup_completes_before_the_lane_drains() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let mut cancellation = registration.cancellation();
    let mutation_started = Arc::new(Notify::new());
    let release_mutation = Arc::new(Notify::new());
    let pending = Arc::new(Notify::new());
    let cleanup_started = Arc::new(Notify::new());
    let release_cleanup = Arc::new(Notify::new());
    let cleanup_entered = Arc::new(AtomicBool::new(false));
    let cleaned = Arc::new(AtomicBool::new(false));
    let task = {
        let mutation_started = Arc::clone(&mutation_started);
        let release_mutation = Arc::clone(&release_mutation);
        let pending = Arc::clone(&pending);
        let cleanup_started = Arc::clone(&cleanup_started);
        let release_cleanup = Arc::clone(&release_cleanup);
        let cleanup_entered = Arc::clone(&cleanup_entered);
        let cleaned = Arc::clone(&cleaned);
        tokio::spawn(run_registered_lifecycle_producer_with_cancellation_cleanup(
            registration,
            async move {
                let mutation = begin_current_lifecycle_mutation().expect("mutation admitted");
                mutation_started.notify_one();
                release_mutation.notified().await;
                drop(mutation);
                pending.notified().await;
            },
            async move {
                cleanup_entered.store(true, Ordering::SeqCst);
                cleanup_started.notify_one();
                release_cleanup.notified().await;
                cleaned.store(true, Ordering::SeqCst);
            },
        ))
    };

    mutation_started.notified().await;
    let close = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_and_wait().await }
    });
    cancellation.cancelled().await;
    tokio::task::yield_now().await;
    assert!(
        !cleanup_entered.load(Ordering::SeqCst),
        "cleanup cannot overlap the producer mutation"
    );
    release_mutation.notify_one();
    cleanup_started.notified().await;
    assert!(
        !close.is_finished(),
        "lane drain must wait for local cleanup"
    );

    release_cleanup.notify_one();
    assert_eq!(task.await.expect("producer joins"), None);
    close.await.expect("closed producer lane drains");
    assert!(cleaned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn successful_producer_does_not_run_its_cancellation_cleanup() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let cleaned = Arc::new(AtomicBool::new(false));
    let output = run_registered_lifecycle_producer_with_cancellation_cleanup(
        registration,
        async { 7_u8 },
        {
            let cleaned = Arc::clone(&cleaned);
            async move {
                cleaned.store(true, Ordering::SeqCst);
            }
        },
    )
    .await;

    assert_eq!(output, Some(7));
    assert!(!cleaned.load(Ordering::SeqCst));
    registry.close_and_wait().await;
}

#[tokio::test]
async fn rejected_mutation_before_watch_delivery_runs_cleanup_exactly_once() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    close_normal_gate_without_notifying_watch(&registry);

    let output = run_registered_lifecycle_producer_with_cancellation_cleanup(
        registration,
        async {
            assert!(
                begin_current_lifecycle_mutation().is_none(),
                "the closed admission gate rejects the mutation"
            );
            11_u8
        },
        {
            let cleanup_count = Arc::clone(&cleanup_count);
            async move {
                cleanup_count.fetch_add(1, Ordering::SeqCst);
            }
        },
    )
    .await;

    assert_eq!(output, None);
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
    registry.close_and_wait().await;
}

#[tokio::test]
async fn rejected_mutation_before_watch_delivery_discards_basic_output() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    close_normal_gate_without_notifying_watch(&registry);

    let output = run_registered_lifecycle_producer(registration, async {
        assert!(
            begin_current_lifecycle_mutation().is_none(),
            "the closed admission gate rejects the mutation"
        );
        13_u8
    })
    .await;

    assert_eq!(output, None);
    registry.close_and_wait().await;
}

#[tokio::test]
async fn completion_without_a_rejected_mutation_survives_the_gate_watch_window() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    close_normal_gate_without_notifying_watch(&registry);

    let output = run_registered_lifecycle_producer_with_cancellation_cleanup(
        registration,
        async { 17_u8 },
        {
            let cleanup_count = Arc::clone(&cleanup_count);
            async move {
                cleanup_count.fetch_add(1, Ordering::SeqCst);
            }
        },
    )
    .await;

    assert_eq!(output, Some(17));
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 0);
    registry.close_and_wait().await;
}

#[tokio::test]
async fn mutation_admitted_before_close_completes_without_cleanup() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let mutation_started = Arc::new(Notify::new());
    let release_mutation = Arc::new(Notify::new());
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(run_registered_lifecycle_producer_with_cancellation_cleanup(
        registration,
        {
            let mutation_started = Arc::clone(&mutation_started);
            let release_mutation = Arc::clone(&release_mutation);
            async move {
                let _mutation =
                    begin_current_lifecycle_mutation().expect("mutation admitted before close");
                mutation_started.notify_one();
                release_mutation.notified().await;
                23_u8
            }
        },
        {
            let cleanup_count = Arc::clone(&cleanup_count);
            async move {
                cleanup_count.fetch_add(1, Ordering::SeqCst);
            }
        },
    ));

    mutation_started.notified().await;
    let close = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_and_wait().await }
    });
    registry.wait_until_normal_closing_for_test().await;
    release_mutation.notify_one();

    assert_eq!(task.await.expect("producer joins"), Some(23));
    close.await.expect("closed producer lane drains");
    assert_eq!(cleanup_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn nested_mutation_guards_keep_the_producer_mutating_until_the_outer_guard_drops() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let outer = registration
        .try_begin_mutation()
        .expect("outer mutation admitted");
    let mut cancellation = registration.cancellation();
    let close = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_and_wait().await }
    });

    cancellation.cancelled().await;
    let inner = registration
        .try_begin_mutation()
        .expect("nested mutation remains part of the admitted scope");
    drop(inner);
    assert!(
        cancellation.is_mutating(),
        "dropping a nested guard must not make the producer pending"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), cancellation.wait_until_pending())
            .await
            .is_err(),
        "shutdown must keep waiting for the outer mutation guard"
    );

    drop(outer);
    cancellation.wait_until_pending().await;
    drop(registration);
    close.await.expect("shutdown drains nested mutation");
}

#[tokio::test]
async fn pre_admitted_task_is_not_polled_before_parent_handoff_completes() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let parent_handoff = registration
        .try_begin_mutation()
        .expect("parent handoff admitted");
    let task_started = Arc::new(Notify::new());
    let was_polled = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(run_registered_lifecycle_producer(registration, {
        let task_started = Arc::clone(&task_started);
        let was_polled = Arc::clone(&was_polled);
        async move {
            was_polled.store(true, Ordering::SeqCst);
            task_started.notify_one();
        }
    }));

    tokio::task::yield_now().await;
    assert!(
        !was_polled.load(Ordering::SeqCst),
        "the child cannot run before its task-owned state is installed"
    );

    drop(parent_handoff);
    task_started.notified().await;
    assert_eq!(task.await.expect("pre-admitted producer joins"), Some(()));
    registry.close_and_wait().await;
}

#[tokio::test]
async fn close_during_parent_handoff_cancels_child_before_first_poll() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let parent_handoff = registration
        .try_begin_mutation()
        .expect("parent handoff admitted");
    let was_polled = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(run_registered_lifecycle_producer(registration, {
        let was_polled = Arc::clone(&was_polled);
        async move {
            was_polled.store(true, Ordering::SeqCst);
        }
    }));
    let close = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_and_wait().await }
    });

    tokio::task::yield_now().await;
    assert!(!close.is_finished(), "close waits for the parent handoff");
    drop(parent_handoff);

    close.await.expect("closed producer lane drains");
    assert_eq!(task.await.expect("cancelled producer joins"), None);
    assert!(
        !was_polled.load(Ordering::SeqCst),
        "lane closure must win before the child receives its start signal"
    );
}

#[tokio::test]
async fn close_during_cleanup_handoff_runs_cleanup_once_before_drain() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let parent_handoff = registration
        .try_begin_mutation()
        .expect("parent handoff admitted");
    let was_polled = Arc::new(AtomicBool::new(false));
    let cleanup_count = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(run_registered_lifecycle_producer_with_cancellation_cleanup(
        registration,
        {
            let was_polled = Arc::clone(&was_polled);
            async move {
                was_polled.store(true, Ordering::SeqCst);
            }
        },
        {
            let cleanup_count = Arc::clone(&cleanup_count);
            async move {
                cleanup_count.fetch_add(1, Ordering::SeqCst);
            }
        },
    ));
    let close = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_and_wait().await }
    });

    tokio::task::yield_now().await;
    assert!(!close.is_finished(), "close waits for the parent handoff");
    drop(parent_handoff);

    close.await.expect("closed producer lane drains");
    assert_eq!(task.await.expect("cancelled producer joins"), None);
    assert!(!was_polled.load(Ordering::SeqCst));
    assert_eq!(
        cleanup_count.load(Ordering::SeqCst),
        1,
        "task-owned cleanup runs exactly once before registration drain"
    );
}

#[tokio::test]
async fn lane_close_wins_against_a_new_mutation_boundary() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let registration = registry.try_register().expect("producer registered");
    let mut cancellation = registration.cancellation();
    let close = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_and_wait().await }
    });

    cancellation.cancelled().await;
    assert!(
        registration.try_begin_mutation().is_none(),
        "a producer observed pending at lane close must not re-enter mutation"
    );
    drop(registration);
    close.await.expect("closed producer lane drains");
}

#[tokio::test]
async fn normal_close_keeps_the_lifecycle_hook_lane_open_until_final_close() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let normal = registry.try_register().expect("normal producer registered");
    let lifecycle_hook = registry
        .try_register_in_lane(LifecycleProducerLane::LifecycleHook)
        .expect("lifecycle-hook producer registered");
    let mut normal_cancellation = normal.cancellation();
    let mut hook_cancellation = lifecycle_hook.cancellation();
    let close_normal = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.close_normal_and_wait().await }
    });

    normal_cancellation.cancelled().await;
    drop(normal);
    close_normal.await.expect("normal producer lane drains");
    assert!(registry.try_register().is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), hook_cancellation.cancelled())
            .await
            .is_err(),
        "normal close must not cancel lifecycle-hook producers"
    );
    let hook_mutation = lifecycle_hook
        .try_begin_mutation()
        .expect("hook lane remains open");
    drop(hook_mutation);
    drop(lifecycle_hook);

    registry.close_and_wait().await;
    assert!(registry
        .try_register_in_lane(LifecycleProducerLane::LifecycleHook)
        .is_none());
}

#[tokio::test]
async fn descendants_inherit_the_current_registered_producer_lane() {
    let registry = Arc::new(LifecycleProducerRegistry::new());
    let hook = registry
        .try_register_in_lane(LifecycleProducerLane::LifecycleHook)
        .expect("lifecycle-hook producer registered");
    let inherited = run_registered_lifecycle_producer(hook, {
        let registry = Arc::clone(&registry);
        async move {
            registry
                .try_register()
                .expect("descendant producer registered")
                .lane
        }
    })
    .await;

    assert_eq!(inherited, Some(LifecycleProducerLane::LifecycleHook));
    registry.close_and_wait().await;
}
