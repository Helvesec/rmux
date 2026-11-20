//! A cancel-safe request that ends its own popup wait when the server shuts down.
//!
//! A waiting CLI popup preserves earlier output and stops its remaining queue, as in tmux 3.7b.
//! Its wait marks it stopping when closing starts; other cancel-safe requests lose their response.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rmux_proto::Request;

tokio::task_local! {
    static SHUTDOWN_ANSWER: ShutdownAnswer;
}

#[cfg(test)]
thread_local! {
    static POPUP_COMPLETION_GATE: std::cell::RefCell<Option<Arc<tokio::sync::Semaphore>>> = const { std::cell::RefCell::new(None) };
    static CONNECTION_SHUTDOWN_OBSERVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn gate_popup_completion_for_test(gate: Arc<tokio::sync::Semaphore>) {
    POPUP_COMPLETION_GATE.with(|slot| *slot.borrow_mut() = Some(gate));
    CONNECTION_SHUTDOWN_OBSERVED.set(false);
}

#[cfg(test)]
pub(crate) fn connection_shutdown_observed_for_test() -> bool {
    CONNECTION_SHUTDOWN_OBSERVED.get()
}

#[cfg(test)]
pub(crate) fn popup_completion_paused_for_test() -> bool {
    POPUP_COMPLETION_GATE.with(|slot| slot.borrow().is_none())
}

#[cfg(test)]
pub(crate) async fn pause_popup_completion_for_test() {
    let gate = POPUP_COMPLETION_GATE.with(|slot| slot.borrow_mut().take());
    if let Some(gate) = gate {
        gate.acquire()
            .await
            .expect("popup completion gate")
            .forget();
    }
}

#[derive(Debug, Default)]
struct AnswerState {
    popup_waiting: bool,
    stopping: bool,
}

/// Shared by a connection and the request it dispatches.
#[derive(Debug, Clone)]
pub(crate) struct ShutdownAnswer(Arc<Mutex<AnswerState>>);

impl ShutdownAnswer {
    /// A slot for requests whose own response carries their results and status.
    pub(super) fn for_request(request: &Request) -> Option<Self> {
        matches!(request, Request::SourceFile(_)).then(|| Self(Arc::default()))
    }

    /// Claims the waiting popup's answer and stops its queue as soon as this connection closes.
    pub(super) fn finish_on_shutdown(&self) -> bool {
        #[cfg(test)]
        CONNECTION_SHUTDOWN_OBSERVED.set(true);
        let mut state = self.state();
        state.stopping |= state.popup_waiting;
        state.stopping
    }

    #[cfg(test)]
    fn finishes_itself(&self) -> bool {
        let state = self.state();
        state.popup_waiting || state.stopping
    }

    fn state(&self) -> MutexGuard<'_, AnswerState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Runs `execution` with `answer` as its shutdown slot, if there is one.
pub(super) async fn scope<F: Future>(answer: Option<ShutdownAnswer>, execution: F) -> F::Output {
    match answer {
        Some(answer) => SHUTDOWN_ANSWER.scope(answer, execution).await,
        None => execution.await,
    }
}

/// Declares that the request in flight waits for a popup and ends that wait itself on shutdown,
/// until the guard is dropped; `None` when its connection would cancel it instead.
pub(crate) fn popup_wait_ends_on_shutdown() -> Option<ShutdownPopupWait> {
    SHUTDOWN_ANSWER
        .try_with(|answer| {
            answer.state().popup_waiting = true;
            ShutdownPopupWait(answer.clone())
        })
        .ok()
}

/// Whether the request in flight stopped its popup wait for a shutdown, so its command queue must
/// run nothing further.
pub(crate) fn request_stopping_for_shutdown() -> bool {
    SHUTDOWN_ANSWER
        .try_with(|answer| answer.state().stopping)
        .unwrap_or(false)
}

/// A popup wait of the request in flight that its shutdown answer depends on.
#[derive(Debug)]
pub(crate) struct ShutdownPopupWait(ShutdownAnswer);

impl ShutdownPopupWait {
    /// The server is closing: the request answers with what it has and runs nothing further.
    pub(crate) fn stop_request(&self) {
        self.0.state().stopping = true;
    }
}

impl Drop for ShutdownPopupWait {
    fn drop(&mut self) {
        self.0.state().popup_waiting = false;
    }
}

#[cfg(test)]
mod tests {
    use rmux_proto::{Request, SourceFileRequest};

    use super::{
        popup_wait_ends_on_shutdown, request_stopping_for_shutdown, scope, ShutdownAnswer,
    };

    fn source_file() -> Request {
        Request::SourceFile(Box::new(SourceFileRequest {
            paths: Vec::new(),
            quiet: false,
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: None,
            stdin: None,
        }))
    }

    #[tokio::test]
    async fn a_request_finishes_itself_only_while_its_popup_waits_or_once_it_stops() {
        let answer = ShutdownAnswer::for_request(&source_file()).expect("source-file has a slot");
        assert!(!answer.finishes_itself(), "nothing to finish before a wait");

        scope(Some(answer.clone()), async {
            let wait = popup_wait_ends_on_shutdown().expect("the connection lets it finish");
            assert!(answer.finishes_itself());
            assert!(!request_stopping_for_shutdown());
            drop(wait);
            assert!(
                !answer.finishes_itself(),
                "a popup wait that completed normally leaves cancellation as before"
            );

            let wait = popup_wait_ends_on_shutdown().expect("the connection lets it finish");
            wait.stop_request();
            drop(wait);
            assert!(request_stopping_for_shutdown(), "the queue stops");
            assert!(
                answer.finishes_itself(),
                "the stopping request still answers"
            );
        })
        .await;
        assert!(
            popup_wait_ends_on_shutdown().is_none(),
            "outside a connection nothing can answer"
        );
    }
}
