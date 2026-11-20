#[test]
fn display_panes_dispatch_completes_on_default_stack() {
    // Isolate a stack overflow so the harness reports an ordinary test failure.
    // Reuse the frozen label/input witness without changing its population.
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "--exact",
            "handler::attach_tests::display_panes::display_panes_input_uses_visible_pane_base_index_labels",
            "--test-threads=1",
        ])
        .env_remove("RUST_MIN_STACK")
        .output()
        .expect("spawn default-stack display-panes witness");
    assert!(
        output.status.success(),
        "display-panes dispatch failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
