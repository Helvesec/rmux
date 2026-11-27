use super::*;

// Include the terminator: a partial OSC must not consume the snapshot bytes.
const READY: &[u8] = b"\x1b]0;RMUX_RESET_READY_151\x07";

pub(super) struct ReplacementChild {
    root: ScratchPath,
    observed: std::sync::atomic::AtomicBool,
}

impl ReplacementChild {
    pub(super) fn new() -> Self {
        let root = temp_root("reset-replacement-child");
        std::fs::create_dir_all(&root).expect("replacement startup gate directory");
        Self {
            root,
            observed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(windows)]
    pub(super) fn command(&self) -> String {
        // A real child produces the observed DEC wrap mutation. The file gate
        // lets the control deliver it while the stale reset cannot execute.
        crate::test_shell::powershell_encoded_command(&format!(
            "while (!(Test-Path -LiteralPath {})) {{ Start-Sleep -Milliseconds 10 }}; \
             $inputStream=[Console]::OpenStandardInput(); \
             [Console]::Write(([char]27).ToString()+'[?7l'+([char]27).ToString()+']0;RMUX_RESET_READY_151'+[char]7); \
             $inputStream.CopyTo([System.IO.Stream]::Null)",
            crate::test_shell::powershell_quote_path(&self.root.join("go")),
        ))
    }

    #[cfg(unix)]
    pub(super) fn command(&self) -> String {
        format!(
            "while [ ! -f {} ]; do sleep 0.01; done; \
             printf '\\033[?7l\\033]0;RMUX_RESET_READY_151\\007'; \
             exec cat >/dev/null",
            crate::test_shell::sh_quote_path(&self.root.join("go")),
        )
    }

    pub(super) async fn prepare_snapshot(&self, output: &crate::pane_io::PaneOutputSender) {
        // Spawn completion and the first ConPTY read precede PowerShell's
        // terminal setup. Observe the child's own ordered output boundary
        // before installing the state that only the queued reset may mutate.
        self.release_and_observe(output).await;
    }

    pub(super) async fn release_and_observe(&self, output: &crate::pane_io::PaneOutputSender) {
        if self.observed.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut receiver = output.subscribe_from_oldest();
        let _demand = output.acquire_read_demand();
        std::fs::write(self.root.join("go"), b"go").expect("release replacement startup");
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut bytes = Vec::new();
            loop {
                if let rmux_core::events::OutputCursorItem::Event(event) = receiver.recv().await {
                    bytes.extend_from_slice(event.bytes());
                    if bytes.windows(READY.len()).any(|part| part == READY) {
                        eprintln!("reset replacement startup boundary observed: {bytes:?}");
                        break;
                    }
                }
            }
        })
        .await
        .expect("replacement publishes its startup boundary");
        self.observed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
