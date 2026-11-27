//! Native argv forwarding for Cargo/rustdoc, beyond cmd.exe's command length limit.
use std::path::Path;
use std::process::Command;

fn main() {
    let executable = std::env::args_os().next().unwrap_or_default();
    let is_rustdoc = Path::new(&executable)
        .file_stem()
        .is_some_and(|name| name == "rustdoc-wrapper");
    let mut command = Command::new(env!("RMUX_IDENTITY_PYTHON"));
    if std::env::var_os("RMUX_NATIVE_INPUT_MANIFEST").is_some() {
        command.args(["-I", "-S", "-B"]);
    }
    command.arg(env!("RMUX_IDENTITY_SCRIPT"));
    if is_rustdoc {
        command.arg("rustdoc").arg(env!("RMUX_IDENTITY_RUSTDOC"));
    } else if Path::new(&executable)
        .file_stem()
        .is_some_and(|name| name == "rustc-wrapper")
    {
        command.arg("rustc");
    } else {
        command
            .arg("build-script")
            .arg(std::env::current_exe().expect("wrapper path"));
    }
    command.args(std::env::args_os().skip(1));
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(_) => {
            eprintln!("candidate identity: native compiler wrapper failed");
            std::process::exit(1);
        }
    }
}
