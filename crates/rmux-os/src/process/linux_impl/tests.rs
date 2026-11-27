use super::command_name_impl;

#[test]
fn linux_command_name_does_not_fall_back_to_comm_without_cmdline() {
    // An unreaped child has released its memory map, so its cmdline is empty
    // while its comm still answers. execve has the same empty-cmdline window,
    // during which comm can be torn ("slee"), so comm must never stand in.
    let mut child = std::process::Command::new("sleep")
        .arg("0")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while super::super::is_live(pid) {
        assert!(std::time::Instant::now() < deadline, "sleep 0 did not exit");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).expect("zombie cmdline");
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).expect("zombie comm");
    assert!(
        cmdline.is_empty(),
        "zombie cmdline should be empty: {cmdline:?}"
    );
    assert_eq!(comm.trim(), "sleep");

    assert_eq!(command_name_impl(pid).expect("read cmdline"), None);
    child.wait().expect("reap sleep");
}
