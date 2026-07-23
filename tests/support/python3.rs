use std::process::Command;

const VERSION_CHECK: &str = "import sys; raise SystemExit(0 if sys.version_info.major == 3 else 1)";

pub(crate) fn command() -> Command {
    for program in ["python3", "python"] {
        if is_python3(program, &[]) {
            return Command::new(program);
        }
    }

    #[cfg(windows)]
    if is_python3("py", &["-3"]) {
        let mut command = Command::new("py");
        command.arg("-3");
        return command;
    }

    panic!("Python 3 is required to run this release fixture");
}

fn is_python3(program: &str, leading_args: &[&str]) -> bool {
    Command::new(program)
        .args(leading_args)
        .args(["-c", VERSION_CHECK])
        .output()
        .is_ok_and(|output| output.status.success())
}
