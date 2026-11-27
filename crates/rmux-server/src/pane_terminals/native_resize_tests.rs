#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::path::{Path, PathBuf};
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName, TerminalSize};
use serde_json::{json, Value};

use super::*;
use crate::pane_terminal_process::PaneTerminal;
use crate::pane_terminals::HandlerState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResizeCase {
    Record,
    ScopedPartial,
    ScopedResidual,
    /// Like `ScopedPartial`, for the pane-transfer path. That path compensates
    /// with a second whole-session resize and returns a plain `RmuxError`
    /// instead of a typed `WindowResizeCompensation`, so these two never reach
    /// `check_resize_compensation_for_test`.
    TransferPartial,
    /// Like `ScopedResidual`, for the pane-transfer path.
    TransferResidual,
    SameSize,
    BeforeBatch,
    Partial,
    Residual,
    Lookup,
}

#[derive(Debug)]
pub(in crate::pane_terminals) struct ResizeProbe {
    root: PathBuf,
    case: ResizeCase,
    calls: Vec<Value>,
    compensation: Option<Value>,
}

impl ResizeProbe {
    pub(super) fn before_call(
        &mut self,
        plan: &NativeResizePlan,
        pane: &SessionPane,
        terminal: &PaneTerminal,
    ) -> Option<rmux_pty::PtyError> {
        let call = self.calls.len() + 1;
        let injected = (matches!(
            self.case,
            ResizeCase::Partial
                | ResizeCase::Residual
                | ResizeCase::ScopedPartial
                | ResizeCase::ScopedResidual
                | ResizeCase::TransferPartial
                | ResizeCase::TransferResidual
        ) && call == 2)
            || (matches!(
                self.case,
                ResizeCase::Residual | ResizeCase::ScopedResidual | ResizeCase::TransferResidual
            ) && call == 3);
        self.calls
            .push(json!({"call":call,"owner":plan.runtime_session.as_str(),
            "pane_id":pane.id.as_u32(),"pid":terminal.pid(),"injected_pre_call":injected,
            "requested":[pane.geometry.cols(),pane.geometry.rows()]}));
        injected.then(|| std::io::Error::other("injected native resize pre-call failure").into())
    }

    pub(super) fn after_call(&mut self, terminal: &PaneTerminal, returned_ok: bool) {
        let row = self.calls.last_mut().unwrap();
        row["returned_ok"] = json!(returned_ok);
        if returned_ok
            && !matches!(
                self.case,
                ResizeCase::Record
                    | ResizeCase::ScopedPartial
                    | ResizeCase::ScopedResidual
                    | ResizeCase::TransferPartial
                    | ResizeCase::TransferResidual
            )
        {
            row["child_ack"] = sample(&self.root, terminal, "native");
            assert_eq!(row["requested"], row["child_ack"]["master_size"]);
        }
    }
}

fn plan_fact(plan: &NativeResizePlan) -> Value {
    json!({"owner":plan.runtime_session.as_str(),"pixels":format!("{:?}",plan.terminal_pixels),
        "panes":plan.panes.iter().map(|p|json!({"id":p.id.as_u32(),"window":p.window_index,
            "index":p.index,"size":[p.geometry.cols(),p.geometry.rows()]})).collect::<Vec<_>>()})
}

fn failed_batch(error: &WindowRuntimeResizeError) -> &NativeResizeFailure {
    let WindowRuntimeResizeError::Native(failure) = error else {
        panic!("expected a maintained native batch: {error:?}");
    };
    failure
}

fn failure_fact(failure: &NativeResizeFailure) -> Value {
    json!({"plan":plan_fact(&failure.plan),"completed_prefix":failure.completed_prefix,
        "stop":format!("{:?}",failure.stop)})
}

impl HandlerState {
    pub(crate) fn arm_resize_outcome_test(&mut self, root: &Path, case: ResizeCase) {
        assert!(self.terminals.resize_probe.is_none());
        self.terminals.resize_probe = Some(ResizeProbe {
            root: root.to_owned(),
            case,
            calls: vec![],
            compensation: None,
        });
        if case == ResizeCase::BeforeBatch {
            self.fail_next_resize_for_test();
        }
    }

    pub(in crate::pane_terminals) fn check_resize_compensation_for_test(
        &mut self,
        error: &WindowMutationResizeError,
    ) {
        let Some(probe) = self.terminals.resize_probe.as_mut() else {
            return;
        };
        let WindowMutationResizeError::Compensation(attempts) = error else {
            panic!("expected separate original and compensation outcomes: {error:?}");
        };
        let original = failed_batch(&attempts.original);
        if matches!(
            probe.case,
            ResizeCase::ScopedPartial | ResizeCase::ScopedResidual
        ) {
            assert_eq!(original.completed_prefix, 1);
            assert!(matches!(original.stop, NativeResizeStop::MayHaveApplied(_)));
            let original_ids = original
                .plan
                .panes
                .iter()
                .map(|pane| pane.id)
                .collect::<Vec<_>>();
            let compensation_fact = match &attempts.compensation {
                Ok(plan) => {
                    assert_eq!(probe.case, ResizeCase::ScopedPartial);
                    assert_eq!(
                        plan.panes.iter().map(|pane| pane.id).collect::<Vec<_>>(),
                        original_ids
                    );
                    json!({"completed":plan_fact(plan)})
                }
                Err(error) => {
                    assert_eq!(probe.case, ResizeCase::ScopedResidual);
                    let failure = failed_batch(error);
                    assert_eq!(failure.completed_prefix, 0);
                    assert_eq!(
                        failure
                            .plan
                            .panes
                            .iter()
                            .map(|pane| pane.id)
                            .collect::<Vec<_>>(),
                        original_ids
                    );
                    json!({"failure":failure_fact(failure)})
                }
            };
            probe.compensation = Some(json!({
                "target":attempts.target.to_string(),
                "original":failure_fact(original),
                "compensation":compensation_fact,
            }));
            return;
        }
        assert_eq!(original.plan.panes.len(), 2);
        assert_eq!(original.plan.panes[0].window_index, 7);
        assert_eq!(original.plan.panes[0].geometry.rows(), 28);
        assert_eq!(original.plan.panes[0].geometry.cols(), 45);
        assert_eq!(original.plan.panes[1].geometry.cols(), 44);
        assert_eq!(
            original.completed_prefix,
            usize::from(probe.case != ResizeCase::BeforeBatch)
        );
        if probe.case == ResizeCase::BeforeBatch {
            assert!(matches!(original.stop, NativeResizeStop::BeforeBatch(_)));
        } else {
            assert!(matches!(original.stop, NativeResizeStop::MayHaveApplied(_)));
        }
        let (compensation_plan, compensation_fact) = match &attempts.compensation {
            Ok(plan) => {
                assert_ne!(probe.case, ResizeCase::Residual);
                (plan, json!({"completed":plan_fact(plan)}))
            }
            Err(error) => {
                assert_eq!(probe.case, ResizeCase::Residual);
                let failure = failed_batch(error);
                assert_eq!(failure.completed_prefix, 0);
                assert!(matches!(failure.stop, NativeResizeStop::MayHaveApplied(_)));
                (&failure.plan, failure_fact(failure))
            }
        };
        assert_eq!(
            compensation_plan.runtime_session,
            original.plan.runtime_session
        );
        assert_eq!(
            compensation_plan
                .panes
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            original.plan.panes.iter().map(|p| p.id).collect::<Vec<_>>()
        );
        assert_eq!(compensation_plan.panes[0].geometry.cols(), 40);
        assert_eq!(compensation_plan.panes[1].geometry.cols(), 39);
        assert_eq!(compensation_plan.panes[0].geometry.rows(), 24);
        probe.compensation = Some(json!({"target":attempts.target.to_string(),
            "original":failure_fact(original),"compensation":compensation_fact}));
    }

    pub(crate) fn finish_resize_outcome_test(&mut self) -> Value {
        let probe = self.terminals.resize_probe.take().unwrap();
        // `TransferPartial`/`TransferResidual` are deliberately absent below:
        // the pane-transfer transaction compensates by re-running
        // `resize_terminals` and propagates a plain `RmuxError`, so no
        // `WindowResizeCompensation` is ever built for them and
        // `check_resize_compensation_for_test` is never reached. Their witness
        // reads the recorded call list instead of the typed helper result.
        if matches!(
            probe.case,
            ResizeCase::BeforeBatch
                | ResizeCase::Partial
                | ResizeCase::Residual
                | ResizeCase::ScopedPartial
                | ResizeCase::ScopedResidual
        ) {
            assert!(
                probe.compensation.is_some(),
                "typed helper result was never consumed"
            );
        }
        json!({"calls":probe.calls,"typed":probe.compensation})
    }

    pub(crate) fn resize_outcome_samples(&self, root: &Path, phase: &str) -> Value {
        let mut rows = vec![];
        for (owner, panes) in &self.terminals.sessions {
            for (id, terminal) in panes {
                let mut row = sample(root, terminal, phase);
                row["pane_id"] = json!(id.as_u32());
                row["owner"] = json!(owner.as_str());
                rows.push(row);
            }
        }
        rows.sort_by_key(|r| r["pane_id"].as_u64().unwrap());
        json!(rows)
    }

    pub(crate) fn resize_outcome_state(&self) -> Value {
        let mut sessions = vec![];
        for (name, session) in self
            .sessions
            .iter()
            .map(|session| (session.name(), session))
        {
            let windows = session.windows().map(|(index, window)| {
                let owner = self.runtime_session_name_for_window(name, index);
                let panes = window.panes().iter().map(|pane| {
                    let life = self.pane_lifecycle(pane.id()).unwrap();
                    let transcript = &self.transcripts[&owner][&pane.id()];
                    let size = transcript.lock().screen().size();
                    json!({"id":pane.id().as_u32(),"size":[pane.geometry().cols(),pane.geometry().rows()],
                        "transcript":[size.cols,size.rows],"lifecycle_size":[life.dimensions().cols,life.dimensions().rows],
                        "lifecycle_session":life.session_id.as_u32(),"revision":life.revision,
                        "lifecycle_generation":life.generation,"output_generation":self.pane_output_generation(&owner,pane.id())})
                }).collect::<Vec<_>>();
                json!({"index":index,"id":window.id().as_u32(),"owner":owner.as_str(),
                    "size":[window.size().cols,window.size().rows],"selected":window.active_pane_index(),"panes":panes})
            }).collect::<Vec<_>>();
            sessions.push(json!({"name":name.as_str(),"id":session.id().as_u32(),"active":session.active_window_index(),"windows":windows}));
        }
        sessions.sort_by_key(|r| r["name"].as_str().unwrap().to_owned());
        json!({"sessions":sessions,"queue":format!("{:?}",self.applied_window_resizes)})
    }

    pub(crate) fn resize_outcome_quit(&self) {
        for panes in self.terminals.sessions.values() {
            for terminal in panes.values() {
                terminal
                    .clone_master()
                    .unwrap()
                    .write_all(b"QUIT\n")
                    .unwrap();
            }
        }
    }

    // A later lookup really fails after an earlier child changed. The suffix
    // deliberately repeats that live pane with a different size: it must not run.
    pub(crate) fn exercise_resize_lookup_test(&mut self, owner: &SessionName) -> Value {
        let mut completed = self.resize_window_terminal_runtime(owner, 0).unwrap();
        let mut missing = completed.panes[1];
        missing.id = PaneId::new(u32::MAX);
        let mut first = completed.panes[0];
        first.geometry = rmux_core::PaneGeometry::new(0, 0, 45, 28);
        let suffix = completed.panes[0];
        completed.panes = vec![first, missing, suffix];
        let failure = self.terminals.resize_session(completed).unwrap_err();
        assert_eq!(failure.completed_prefix, 1);
        assert!(matches!(failure.stop, NativeResizeStop::MissingPane));
        assert_eq!(failure.plan.panes[1].id, missing.id);
        assert_eq!(failure.plan.panes[2].geometry.cols(), 40);
        let fact = failure_fact(&failure);
        assert_eq!(
            RmuxError::from(*failure).to_string(),
            format!("server error: missing pane terminal for {owner}:0.1")
        );
        // Session-wide sibling still consumes the returned plan and converts errors.
        self.fail_next_resize_for_test();
        assert_eq!(
            self.resize_terminals(owner).unwrap_err().to_string(),
            "server error: injected pane terminal resize failure"
        );
        self.resize_terminals(owner).unwrap();
        fact
    }
}

pub(super) fn sample(root: &Path, terminal: &PaneTerminal, phase: &str) -> Value {
    static TOKEN: AtomicU64 = AtomicU64::new(1);
    let token = format!("{phase}-{}", TOKEN.fetch_add(1, Ordering::Relaxed));
    let bytes = format!("{token}\n");
    let master = terminal.clone_master().unwrap();
    master.write_all(bytes.as_bytes()).unwrap();
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        if let Ok(contents) =
            std::fs::read_to_string(root.join(format!("child-{}.jsonl", terminal.pid())))
        {
            for line in contents.lines() {
                if let Ok(mut row) = serde_json::from_str::<Value>(line) {
                    if row["token"] == token {
                        let size = master.size().unwrap();
                        row["master_size"] = json!([size.cols, size.rows]);
                        row["sent_hex"] = json!(bytes
                            .as_bytes()
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>());
                        assert_eq!(row["stdin_hex"], row["sent_hex"]);
                        assert_eq!(json!([row["cols"], row["rows"]]), row["master_size"]);
                        return row;
                    }
                }
            }
        }
        assert!(
            Instant::now() < until,
            "child {} did not ACK {token}",
            terminal.pid()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn absent_session_has_no_completed_call_and_retains_supplied_plan() {
    let owner = SessionName::new("absent-native-owner").unwrap();
    let pane = SessionPane {
        id: PaneId::new(41),
        window_index: 7,
        index: 3,
        geometry: rmux_core::PaneGeometry::new(0, 0, 40, 24),
        window_size: TerminalSize { cols: 80, rows: 24 },
    };
    let mut store = PaneTerminalStore::default();
    let failure = store
        .resize_session(NativeResizePlan {
            runtime_session: owner,
            panes: vec![pane],
            terminal_pixels: None,
        })
        .unwrap_err();
    assert_eq!(failure.completed_prefix, 0);
    assert!(matches!(failure.stop, NativeResizeStop::BeforeBatch(_)));
    assert_eq!(failure.plan.panes[0].id, pane.id);
    assert_eq!(
        RmuxError::from(*failure).to_string(),
        "server error: missing pane terminals for session absent-native-owner"
    );
}

#[test]
fn preparation_and_model_errors_do_not_enter_native_compensation() {
    let owner = SessionName::new("resize-preparation").unwrap();
    let mut state = HandlerState::default();
    state.fail_next_resize_for_test();
    let error = state.resize_window_terminal_runtime(&owner, 0).unwrap_err();
    assert!(matches!(error, WindowRuntimeResizeError::Preparation(_)));
    assert_eq!(state.terminals.fail_next_resizes, 1);

    let size = TerminalSize { cols: 80, rows: 24 };
    state.sessions.create_session(owner.clone(), size).unwrap();
    let error = state
        .mutate_session_and_resize_window_terminal_with_family_if(
            &owner,
            0,
            |session| {
                session.resize_window(0, TerminalSize { cols: 90, rows: 28 })?;
                Err::<(), _>(RmuxError::Server("model mutation failed".into()))
            },
            |_| true,
        )
        .unwrap_err();
    assert!(matches!(error, WindowMutationResizeError::BeforeResize(_)));
    assert_eq!(state.terminals.fail_next_resizes, 1);
    assert_eq!(
        state.sessions.session(&owner).unwrap().window().size(),
        size
    );
}
