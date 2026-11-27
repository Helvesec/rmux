use super::*;

#[test]
fn legacy_alias_preserves_assignment_in_a_later_command_group() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("legacy-alias-later-assignment")?;
    let server = spawn_same_wire_server_expecting_raw_alias(
        harness.socket_path(),
        b"command-alias[20] \"probe=display-message -p '$FOO'\"\n",
        "display-message -p first ; FOO=x probe",
        "first\nx\n",
    )?;
    let output = harness.run(&["display-message", "-p", "first", ";", "FOO=x", "probe"])?;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "first\nx\n");
    assert!(stderr(&output).is_empty());
    server.join().expect("fake server exits")?;
    Ok(())
}

#[test]
fn legacy_alias_assignment_cannot_hide_negotiated_mutations() -> Result<(), Box<dyn Error>> {
    for (alias, message) in [
        (
            "probe=new-pane -t %1 -x30 -y8",
            "new-pane requires negotiated execution",
        ),
        (
            "probe=set-pane-placement -t %1 floating",
            "set-pane-placement requires negotiated execution",
        ),
        (
            "probe=resize-pane -t %1 -X3",
            "resize-pane -X/-Y requires negotiated execution",
        ),
    ] {
        let harness = CliHarness::new("legacy-alias-negotiated-mutation")?;
        prepare_fake_server_socket_parent(harness.socket_path())?;
        let listener = UnixListener::bind(harness.socket_path())?;
        let snapshot = format!("command-alias[20] \"{alias}\"\n").into_bytes();
        let server = thread::spawn(move || -> io::Result<()> {
            let mut stream = serve_same_wire_handshake_and_alias_snapshot(&listener, snapshot)?;
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte)?, 0, "no command may be dispatched");
            listener.set_nonblocking(true)?;
            assert!(listener.accept().is_err(), "no fallback connection");
            Ok(())
        });
        let output = harness.run(&["display-message", "-p", "first", ";", "FOO=x", "probe"])?;
        assert_eq!(output.status.code(), Some(1));
        assert!(stdout(&output).is_empty());
        assert!(stderr(&output).contains(message), "{}", stderr(&output));
        server.join().expect("fake server exits")?;
    }
    Ok(())
}
