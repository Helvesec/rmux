//! Canonical canonical workflow fixture shared by integration tests.

/// Number of steps in the canonical canonical workflow.
pub(crate) const SESSION_WORKFLOW_STEP_COUNT: usize = 20;

/// Workflow truecolor terminal-features append payload.
pub(crate) const WORKFLOW_TRUECOLOR_FEATURES: &str =
    ",xterm-256color:RGB,tmux-256color:RGB,screen-256color:RGB,screen:RGB";

/// Shell command used to prove the pane inherited the session COLORTERM value.
pub(crate) const WORKFLOW_COLORTERM_PRINT_COMMAND: &str = "printf \"%s\\n\" \"$COLORTERM\"";

pub(crate) struct WorkflowStep {
    /// Stable step label used by fixture verification and test assertions.
    pub(crate) label: &'static str,
    /// The CLI argv for this step. Steps that require runtime parameterization
    /// (e.g. the hook command path) use `runtime_argv: true` and the test
    /// runner must construct the real argv from `label` alone.
    pub(crate) argv: &'static [&'static str],
    /// When true, `argv` is a template only — the test runner must supply
    /// the real arguments at runtime (e.g. to inject a temp-dir path).
    pub(crate) runtime_argv: bool,
}

/// The ordered canonical canonical workflow fixture.
///
/// Every step's `argv[0]` is a valid rmux subcommand. Steps with
/// `runtime_argv == true` require the test to construct the actual
/// command-line arguments (the stored `argv` shows the command shape
/// but contains default_value values).
pub(crate) const CANONICAL_SESSION_WORKFLOW: [WorkflowStep; SESSION_WORKFLOW_STEP_COUNT] = [
    WorkflowStep {
        label: "cleanup",
        argv: &["kill-session", "-t", "workflow"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "new-session",
        argv: &[
            "new-session",
            "-d",
            "-s",
            "workflow",
            "-x",
            "200",
            "-y",
            "50",
        ],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "has-session",
        argv: &["has-session", "-t", "workflow"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "status-off",
        argv: &["set-option", "-g", "status", "off"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "terminal-features",
        argv: &[
            "set-option",
            "-as",
            "terminal-features",
            WORKFLOW_TRUECOLOR_FEATURES,
        ],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "session-environment",
        argv: &[
            "set-environment",
            "-t",
            "workflow",
            "COLORTERM",
            "truecolor",
        ],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "client-attached-hook",
        argv: &[
            "set-hook",
            "-t",
            "workflow",
            "client-attached",
            "<HOOK_COMMAND>",
        ],
        runtime_argv: true,
    },
    WorkflowStep {
        label: "split-window-1",
        argv: &["split-window", "-v", "-t", "workflow"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "split-window-2",
        argv: &["split-window", "-v", "-t", "workflow"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "split-window-3",
        argv: &["split-window", "-v", "-t", "workflow"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "select-layout",
        argv: &["select-layout", "-t", "workflow:0", "main-vertical"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "resize-pane",
        argv: &["resize-pane", "-t", "workflow:0.0", "-x", "34"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "select-pane",
        argv: &["select-pane", "-t", "workflow:0.1"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "attach-session",
        argv: &["attach-session", "-t", "workflow"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "send-keys-env",
        argv: &[
            "send-keys",
            "-t",
            "workflow:0.1",
            WORKFLOW_COLORTERM_PRINT_COMMAND,
            "Enter",
        ],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "send-keys-sleep",
        argv: &["send-keys", "-t", "workflow:0.1", "sleep 5", "Enter"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "send-keys-ctrl-c",
        argv: &["send-keys", "-t", "workflow:0.1", "C-c"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "detach-client",
        argv: &["detach-client"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "kill-session",
        argv: &["kill-session", "-t", "workflow"],
        runtime_argv: false,
    },
    WorkflowStep {
        label: "has-session-after-kill",
        argv: &["has-session", "-t", "workflow"],
        runtime_argv: false,
    },
];

/// Expected labels in canonical order, used for self-verification.
pub(crate) const EXPECTED_LABELS: [&str; SESSION_WORKFLOW_STEP_COUNT] = [
    "cleanup",
    "new-session",
    "has-session",
    "status-off",
    "terminal-features",
    "session-environment",
    "client-attached-hook",
    "split-window-1",
    "split-window-2",
    "split-window-3",
    "select-layout",
    "resize-pane",
    "select-pane",
    "attach-session",
    "send-keys-env",
    "send-keys-sleep",
    "send-keys-ctrl-c",
    "detach-client",
    "kill-session",
    "has-session-after-kill",
];

/// The known set of rmux subcommands that appear in the workflow.
const KNOWN_SUBCOMMANDS: &[&str] = &[
    "kill-session",
    "new-session",
    "has-session",
    "set-option",
    "set-environment",
    "set-hook",
    "split-window",
    "select-layout",
    "resize-pane",
    "select-pane",
    "attach-session",
    "send-keys",
    "detach-client",
];

/// Verifies the canonical workflow fixture is internally coherent.
///
/// This is called from a single integration test to avoid running duplicate
/// verification across every test crate that includes `mod common`.
pub(crate) fn verify_fixture_coherence() {
    // Exactly the expected number of steps.
    assert_eq!(
        CANONICAL_SESSION_WORKFLOW.len(),
        SESSION_WORKFLOW_STEP_COUNT
    );

    // Labels match the expected canonical sequence.
    let actual_labels: Vec<&str> = CANONICAL_SESSION_WORKFLOW
        .iter()
        .map(|step| step.label)
        .collect();
    assert_eq!(actual_labels.as_slice(), EXPECTED_LABELS);

    // Every step has a non-empty argv starting with a known subcommand.
    for step in &CANONICAL_SESSION_WORKFLOW {
        assert!(
            !step.argv.is_empty(),
            "step '{}' must have a non-empty argv",
            step.label
        );
        assert!(
            KNOWN_SUBCOMMANDS.contains(&step.argv[0]),
            "step '{}' argv[0] '{}' is not a known rmux subcommand",
            step.label,
            step.argv[0]
        );
    }

    // Only the hook step requires runtime argv substitution.
    let runtime_steps: Vec<&str> = CANONICAL_SESSION_WORKFLOW
        .iter()
        .filter(|step| step.runtime_argv)
        .map(|step| step.label)
        .collect();
    assert_eq!(
        runtime_steps,
        vec!["client-attached-hook"],
        "only the hook step should require runtime argv substitution"
    );

    // Bookends: begins with cleanup and ends with the post-kill has-session check.
    assert_eq!(CANONICAL_SESSION_WORKFLOW[0].label, "cleanup");
    assert_eq!(CANONICAL_SESSION_WORKFLOW[0].argv[0], "kill-session");
    assert_eq!(
        CANONICAL_SESSION_WORKFLOW[SESSION_WORKFLOW_STEP_COUNT - 1].label,
        "has-session-after-kill"
    );
    assert_eq!(
        CANONICAL_SESSION_WORKFLOW[SESSION_WORKFLOW_STEP_COUNT - 1].argv[0],
        "has-session"
    );

    // Spec-mandated resize-pane geometry.
    let resize = CANONICAL_SESSION_WORKFLOW
        .iter()
        .find(|step| step.label == "resize-pane")
        .expect("fixture must contain resize-pane");
    assert_eq!(
        resize.argv,
        &["resize-pane", "-t", "workflow:0.0", "-x", "34"]
    );

    // Spec-mandated new-session 200x50 geometry.
    let new_session = CANONICAL_SESSION_WORKFLOW
        .iter()
        .find(|step| step.label == "new-session")
        .expect("fixture must contain new-session");
    assert!(
        new_session.argv.contains(&"-x")
            && new_session.argv.contains(&"200")
            && new_session.argv.contains(&"-y")
            && new_session.argv.contains(&"50"),
        "new-session must use 200x50 geometry"
    );

    // The environment-print send-keys step must include the Enter named key.
    let send_keys_env = CANONICAL_SESSION_WORKFLOW
        .iter()
        .find(|step| step.label == "send-keys-env")
        .expect("fixture must contain send-keys-env");
    assert!(
        send_keys_env.argv.contains(&"Enter"),
        "send-keys-env must include the Enter named key"
    );
    assert_eq!(
        send_keys_env.argv,
        &[
            "send-keys",
            "-t",
            "workflow:0.1",
            WORKFLOW_COLORTERM_PRINT_COMMAND,
            "Enter",
        ]
    );

    let send_keys_ctrl_c = CANONICAL_SESSION_WORKFLOW
        .iter()
        .find(|step| step.label == "send-keys-ctrl-c")
        .expect("fixture must contain send-keys-ctrl-c");
    assert_eq!(
        send_keys_ctrl_c.argv,
        &["send-keys", "-t", "workflow:0.1", "C-c"]
    );

    // The canonical workflow uses a plain persistent hook form only; it must
    // not claim an unverified one-shot selector spelling.
    let hook = CANONICAL_SESSION_WORKFLOW
        .iter()
        .find(|step| step.label == "client-attached-hook")
        .expect("fixture must contain client-attached-hook");
    assert_eq!(
        hook.argv,
        &[
            "set-hook",
            "-t",
            "workflow",
            "client-attached",
            "<HOOK_COMMAND>",
        ]
    );

    let session_environment = CANONICAL_SESSION_WORKFLOW
        .iter()
        .find(|step| step.label == "session-environment")
        .expect("fixture must contain session-environment");
    assert_eq!(
        session_environment.argv,
        &[
            "set-environment",
            "-t",
            "workflow",
            "COLORTERM",
            "truecolor",
        ]
    );

    let terminal_features = CANONICAL_SESSION_WORKFLOW
        .iter()
        .find(|step| step.label == "terminal-features")
        .expect("fixture must contain terminal-features");
    assert_eq!(
        terminal_features.argv,
        &[
            "set-option",
            "-as",
            "terminal-features",
            WORKFLOW_TRUECOLOR_FEATURES,
        ]
    );

    let split_steps = CANONICAL_SESSION_WORKFLOW
        .iter()
        .filter(|step| step.argv.first() == Some(&"split-window"))
        .count();
    assert_eq!(
        split_steps, 3,
        "workflow must perform three vertical splits"
    );

    // Required option and environment steps must be present.
    assert!(actual_labels.contains(&"status-off"), "must set status off");
    assert!(
        actual_labels.contains(&"terminal-features"),
        "must append terminal-features"
    );
    assert!(
        actual_labels.contains(&"session-environment"),
        "must set session environment"
    );
    assert!(
        actual_labels.contains(&"has-session-after-kill"),
        "must verify the session is absent after kill-session"
    );
}
