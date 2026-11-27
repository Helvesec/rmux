use super::*;

#[test]
fn pane_shell_command_keeps_scripts_in_shell_and_reads_the_complete_program_word(
) -> Result<(), Box<dyn Error>> {
    let profile = TerminalProfile {
        cwd: std::env::temp_dir(),
        shell: PathBuf::from(default_shell_string()),
        raw_environment: std::sync::Arc::new(Vec::new()),
    };
    for (command, expected) in [
        (r#""/usr"/bin/true"#, "true"),
        (r#"/usr/"bin"/true -x"#, "true"),
        (r#"  "/opt/my tools"/bin/"my tool" -x"#, "my tool"),
        (r#""/usr/bin/"true -x"#, "true"),
        (r#"/usr/bin/tr"u"e -x"#, "true"),
    ] {
        let command = ProcessCommand::Shell(command.to_owned());
        assert_eq!(
            profile.automatic_window_name(Some(&command)).as_deref(),
            Some(expected),
            "initial name for {command:?}"
        );
        assert_eq!(
            profile.runtime_window_name(Some(&command)).as_deref(),
            Some(expected),
            "current name for {command:?}"
        );
    }

    #[cfg(windows)]
    {
        let directory = unique_directory("shell-script-routing")?;
        let profile = TerminalProfile {
            cwd: directory.clone(),
            shell: PathBuf::from("pwsh.exe"),
            raw_environment: std::sync::Arc::new(Vec::new()),
        };
        let result = (|| -> Result<(), Box<dyn Error>> {
            for extension in ["ps1", "PS1", "py", "cmd", "bat", "", "exe", "COM"] {
                let path = directory.join("a").with_extension(extension);
                fs::write(&path, "exit 0")?;
                let text = path.to_string_lossy().into_owned();
                let command = ProcessCommand::Shell(text.clone());
                let actual = super::super::spawn_command(&profile, Some(&command))?;
                let expected = if matches!(extension, "exe" | "COM") {
                    rmux_pty::ChildCommand::new(&path)
                } else {
                    rmux_pty::ChildCommand::new("pwsh.exe").args([
                        "-NoProfile".to_owned(),
                        "-Command".to_owned(),
                        format!(
                            "Set-Location -LiteralPath '{}'; {text}",
                            directory.to_string_lossy().replace('\'', "''")
                        ),
                    ])
                };
                assert_eq!(
                    format!("{actual:?}"),
                    format!("{expected:?}"),
                    "Shell routing for {command:?}"
                );
            }
            Ok(())
        })();
        fs::remove_dir_all(directory)?;
        result?;
    }
    Ok(())
}

#[test]
fn pane_program_path_with_spaces_keeps_its_name_and_windows_launch() -> Result<(), Box<dyn Error>> {
    let mut options = OptionStore::new();
    options.set(
        ScopeSelector::Global,
        OptionName::DefaultShell,
        default_shell_string(),
        SetOptionMode::Replace,
    )?;
    let profile = TerminalProfile::for_session(
        &EnvironmentStore::new(),
        &options,
        &SessionName::new("program-path-spaces")?,
        7,
        &temp_socket_path(),
        None,
        true,
        None,
        None,
        None,
    )?;

    #[cfg(unix)]
    let program = PathBuf::from("/opt/my tools/bin/tool");
    #[cfg(windows)]
    let directory = unique_directory("Program Files")?;
    #[cfg(windows)]
    let program = directory.join("rmux-path-probe.exe");
    #[cfg(windows)]
    fs::copy(
        PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot missing")?)
            .join("System32")
            .join("whoami.exe"),
        &program,
    )?;

    let check_program_path = || -> Result<(), Box<dyn Error>> {
        #[cfg(windows)]
        {
            let command = ProcessCommand::Shell(program.to_string_lossy().into_owned());
            let (master, mut child) = super::super::spawn_pane_process(
                &rmux_pty::ProcessShell,
                PtyTerminalSize::new(100, 30),
                &profile,
                Some(&command),
            )?;
            let status = child.wait()?;
            drop(master);
            assert!(status.success(), "lone program path failed: {status}");
        }

        let expected = program.file_name().unwrap().to_str().unwrap();
        let mut commands = vec![
            ProcessCommand::Shell(format!("\"{}\" -x", program.display())),
            ProcessCommand::Shell(format!("  \"{}\" -x", program.display())),
            ProcessCommand::Argv(vec![program.to_string_lossy().into_owned(), "-x".into()]),
        ];
        if cfg!(windows) {
            commands.push(ProcessCommand::Shell(
                program.to_string_lossy().into_owned(),
            ));
        }
        for command in commands {
            assert_eq!(
                profile.automatic_window_name(Some(&command)).as_deref(),
                Some(expected),
                "initial name for {command:?}"
            );
            assert_eq!(
                profile.runtime_window_name(Some(&command)).as_deref(),
                Some(expected),
                "current name for {command:?}"
            );
        }
        Ok(())
    };
    let result = check_program_path();
    #[cfg(windows)]
    fs::remove_dir_all(directory)?;
    result
}
