//! Windows-only instrumentation for the three public consumer launch sites.
#[path = "native_consumer_inputs.rs"]
mod native_consumer_inputs;
#[path = "native_windows_process.rs"]
mod native_windows_process;

use native_consumer_inputs::{qualified_file, string, variable, write_once, Environment, Manifest};
use native_windows_process::{current, now_ns, same_path, NativeError, Result};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

pub(super) fn enabled(command: &Command) -> bool {
    std::env::var_os("RMUX_NATIVE_INPUT_MANIFEST").is_some()
        || command.get_envs().any(|(key, value)| {
            key.as_encoded_bytes()
                .eq_ignore_ascii_case(b"RMUX_NATIVE_INPUT_MANIFEST")
                && value.is_some()
        })
}

fn environment(command: &Command) -> Environment {
    // All three callers create inheriting Commands. Materialize once; the
    // checker and the real child receive exactly this same environment.
    let mut env: Environment = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        env.retain(|existing, _| {
            !existing
                .as_encoded_bytes()
                .eq_ignore_ascii_case(key.as_encoded_bytes())
        });
        if let Some(value) = value {
            env.insert(key.to_owned(), value.to_owned());
        }
    }
    env
}

struct OwnedChild {
    child: Child,
    waited: bool,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.waited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn observed_output(
    command: &mut Command,
    input: Option<&[u8]>,
    boundary: &Path,
    before: &Value,
    entry: &str,
    owner: &str,
) -> Result<Output> {
    let parent = current()?;
    let executable = Path::new(string(before, "executable")?);
    if !same_path(Path::new(command.get_program()), executable)
        || qualified_file(before, executable)? != string(before, "executable_sha256")?
    {
        return Err(NativeError::Contract(
            "Rust launch differs from qualified image",
        ));
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0000_0004);
    // Suspended creation closes the fast-exit race: observe the original
    // process handle's birth/image before allowing any child code to run.
    let mut process = OwnedChild {
        child: command.spawn()?,
        waited: false,
    };
    let child = native_windows_process::child_identity(&process.child)?;
    if !same_path(&child.image, executable) {
        return Err(NativeError::Contract(
            "Rust child handle image differs before resume",
        ));
    }
    write_once(
        &boundary.join("launch.json"),
        &json!({"owner":owner,"entry":entry,
        "parent":parent.record(),"child":child.record(),"head":before["head"],
        "case":before["case"],"observed_ns":now_ns()}),
    )?;
    let mut stdout = process
        .child
        .stdout
        .take()
        .ok_or(NativeError::Contract("missing child stdout"))?;
    let mut stderr = process
        .child
        .stderr
        .take()
        .ok_or(NativeError::Contract("missing child stderr"))?;
    let stdin = process.child.stdin.take();
    native_windows_process::resume(&process.child)?;
    let (wait, output, errors, written) = std::thread::scope(|scope| {
        let output = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let errors = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let written = scope.spawn(move || -> std::io::Result<()> {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin.write_all(input)?;
            }
            Ok(())
        });
        let wait = native_windows_process::wait(&mut process.child, &child);
        if wait.is_err() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
        (wait, output.join(), errors.join(), written.join())
    });
    let wait = wait?;
    process.waited = true;
    if !current()?.same_life(&parent) {
        return Err(NativeError::Contract("Rust parent lifetime changed"));
    }
    let receipt = json!({"os":"windows","owner":owner,"entry":entry,
        "head":before["head"],"case":before["case"],"parent":parent.record(),
        "child":wait.child.record(),"handle":wait.handle,
        "executable_sha256":before["executable_sha256"],"before_ns":before["at_ns"],
        "manifest":before["manifest"],"manifest_sha256":before["manifest_sha256"],
        "wait_started_ns":wait.started,"wait_finished_ns":wait.finished,
        "wait_api":"WaitForSingleObject","wait_return":wait.returned,"exit":wait.exit,
        "rust_wait_exit":wait.rust_exit,"rust_wait_finished_ns":wait.rust_finished});
    write_once(&boundary.join("wait.json"), &receipt)?;
    let stdout = output.map_err(|_| NativeError::Contract("stdout reader panicked"))??;
    let stderr = errors.map_err(|_| NativeError::Contract("stderr reader panicked"))??;
    written.map_err(|_| NativeError::Contract("stdin writer panicked"))??;
    Ok(Output {
        status: wait.status,
        stdout,
        stderr,
    })
}

fn native_output(command: &mut Command, entry: &str) -> Result<Output> {
    let env = environment(command);
    if std::env::var_os("RMUX_NATIVE_INPUT_MANIFEST").is_some() {
        for key in [
            "RMUX_NATIVE_INPUT_MANIFEST",
            "RMUX_NATIVE_INPUT_MANIFEST_SHA256",
        ] {
            let inherited = std::env::var_os(key)
                .ok_or(NativeError::Contract("Rust parent lost its native session"))?;
            if inherited != variable(&env, key)? {
                return Err(NativeError::Contract(
                    "Rust child changed its parent's native session",
                ));
            }
        }
    }
    let manifest = Manifest::read(&env)?;
    let python = Path::new(variable(&env, "RMUX_IDENTITY_PYTHON")?);
    let script = Path::new(variable(&env, "RMUX_IDENTITY_SCRIPT")?);
    let expected_script =
        Path::new(string(&manifest.record, "source")?).join("scripts/candidate_identity.py");
    if !same_path(script, &expected_script) {
        return Err(NativeError::Contract(
            "Rust checker belongs to another source",
        ));
    }
    let (bootstrap, before) = manifest.bootstrap(python, script)?;
    let args: Vec<OsString> = std::iter::once(command.get_program().to_owned())
        .chain(command.get_args().map(ToOwned::to_owned))
        .collect();
    let args: Vec<&str> = args
        .iter()
        .map(|arg| {
            arg.to_str()
                .ok_or(NativeError::Contract("native consumer argv is not Unicode"))
        })
        .collect::<Result<_>>()?;
    let cwd = command
        .get_current_dir()
        .map(Path::to_owned)
        .unwrap_or(std::env::current_dir()?);
    let request = serde_json::to_vec(&json!({"args":args,"cwd":cwd}))?;
    let mut checker = Command::new(python);
    checker
        .args(["-I", "-S", "-B"])
        .arg(script)
        .arg("rust-consumer-before")
        .current_dir(&cwd)
        .env_clear()
        .envs(&env);
    manifest.check(&env)?;
    let checked = observed_output(
        &mut checker,
        Some(&request),
        &bootstrap,
        &before,
        entry,
        "rust-consumer-checker",
    )?;
    if !checked.status.success() {
        return Err(NativeError::Contract(
            "native consumer before-use checker refused launch",
        ));
    }
    let (boundary, before) = manifest.launch_boundary(serde_json::from_slice(&checked.stdout)?)?;
    manifest.check(&env)?;
    command.env_clear().envs(&env);
    observed_output(command, None, &boundary, &before, entry, "rust-consumer")
}

pub(super) fn output(command: &mut Command, entry: &str) -> std::io::Result<Output> {
    native_output(command, entry).map_err(|error| std::io::Error::other(error.to_string()))
}
