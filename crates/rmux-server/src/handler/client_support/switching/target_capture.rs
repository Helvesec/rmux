use super::*;

tokio::task_local! {
    static SWITCH_CLIENT_TARGET_CAPTURE: RefCell<SwitchClientTargetCaptureState>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::handler) struct SwitchClientTargetCapture {
    pub(in crate::handler) targeted_client: Option<SwitchManagedClientIdentity>,
    pub(in crate::handler) committed_target: Option<AttachedSwitchCommittedTarget>,
}

#[derive(Default)]
struct SwitchClientTargetCaptureState {
    targeted_client: Option<SwitchManagedClientIdentity>,
    committed_target: Option<AttachedSwitchCommittedTarget>,
}

pub(in crate::handler) async fn capture_switch_client_target_identity<T, F>(
    future: F,
) -> (T, SwitchClientTargetCapture)
where
    F: Future<Output = T>,
{
    SWITCH_CLIENT_TARGET_CAPTURE
        .scope(
            RefCell::new(SwitchClientTargetCaptureState::default()),
            async move {
                let output = future.await;
                let captured = SWITCH_CLIENT_TARGET_CAPTURE.with(|captured| {
                    let captured = captured.borrow();
                    SwitchClientTargetCapture {
                        targeted_client: captured.targeted_client,
                        committed_target: captured.committed_target.clone(),
                    }
                });
                (output, captured)
            },
        )
        .await
}

pub(super) fn record_switch_client_target_identity(identity: SwitchManagedClientIdentity) {
    let _ = SWITCH_CLIENT_TARGET_CAPTURE.try_with(|captured| {
        let mut captured = captured.borrow_mut();
        if captured.targeted_client.is_none() {
            captured.targeted_client = Some(identity);
        }
    });
}

pub(super) fn record_switch_client_committed_target(
    client: SwitchManagedClientIdentity,
    target: AttachedSwitchCommittedTarget,
) {
    let _ = SWITCH_CLIENT_TARGET_CAPTURE.try_with(|captured| {
        let mut captured = captured.borrow_mut();
        if captured.targeted_client == Some(client) && captured.committed_target.is_none() {
            captured.committed_target = Some(target);
        }
    });
}

#[cfg(test)]
#[tokio::test]
async fn switch_client_target_capture_is_first_write_wins() {
    let direct = SwitchManagedClientIdentity::Attach {
        pid: 101,
        attach_id: 7,
    };
    let nested = SwitchManagedClientIdentity::Attach {
        pid: 202,
        attach_id: 9,
    };
    let direct_target = AttachedSwitchCommittedTarget {
        target: PaneTarget::with_window(SessionName::new("direct").expect("valid session"), 1, 2),
        session_id: SessionId::new(3),
        window_id: WindowId::new(4),
        pane_id: PaneId::new(5),
    };
    let nested_target = AttachedSwitchCommittedTarget {
        target: PaneTarget::with_window(SessionName::new("nested").expect("valid session"), 6, 7),
        session_id: SessionId::new(8),
        window_id: WindowId::new(9),
        pane_id: PaneId::new(10),
    };
    let ((), captured) = capture_switch_client_target_identity(async {
        record_switch_client_target_identity(direct);
        record_switch_client_target_identity(nested);
        record_switch_client_committed_target(nested, nested_target);
        record_switch_client_committed_target(direct, direct_target.clone());
    })
    .await;
    assert_eq!(
        captured,
        SwitchClientTargetCapture {
            targeted_client: Some(direct),
            committed_target: Some(direct_target),
        }
    );
}

#[cfg(test)]
#[tokio::test]
async fn switch_client_target_capture_keeps_identity_without_a_committed_target() {
    let direct = SwitchManagedClientIdentity::Attach {
        pid: 303,
        attach_id: 11,
    };
    let ((), captured) = capture_switch_client_target_identity(async {
        record_switch_client_target_identity(direct);
    })
    .await;
    assert_eq!(
        captured,
        SwitchClientTargetCapture {
            targeted_client: Some(direct),
            committed_target: None,
        }
    );
}
