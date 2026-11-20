//! Builds the Windows final-sink pane child from pinned Rust source.
//!
//! The child cannot be this test binary re-executed: putting the pane console
//! into raw mode needs FFI and the crate is `#![forbid(unsafe_code)]`. The
//! previous child was therefore a PowerShell script that re-implemented the
//! historical probe's read boundary by hand — which is precisely what left the
//! R1 diagnostic ambiguous, because a hand-written `ReadConsoleW` loop has no
//! demonstrated equivalent for the standard library's request sizing,
//! incomplete-UTF-8 buffering, partial returns or unpaired-surrogate rejection.
//!
//! So the child is the real thing instead: [`CHILD_MAIN_SOURCE`] hands
//! `std::io::stdin().lock()` to [`super::byte_observer`], and both files are
//! written out and compiled with the workspace-pinned toolchain the first time a
//! slot needs a child. `OBSERVER_SOURCE` is `include_str!` of the very module
//! this test binary compiles and drives, so the observer the harness asserts on
//! and the observer the child runs are the same bytes by construction.
//!
//! # Why every build is fresh and private
//!
//! The whole value of this child is that the R1 A/B can say *which* observer it
//! measured. The build this replaces cached the executable under a predictable
//! shared path and returned it whenever `rmux-final-sink-child.exe` there
//! happened to be a file — so a stale, half-installed or deliberately planted
//! executable was executed as the pinned child, and the diagnostic would have
//! reported on whatever that was.
//!
//! A path is not provenance, and neither is anything stored beside an
//! executable: a file the harness does not control cannot vouch for a file the
//! harness does not control. The correction is therefore not a stronger cache
//! but no cache at all. Each process compiles its own child into a directory it
//! created exclusively, never adopting an existing one, and [`OnceLock`] is an
//! in-process compile-once guard over *that* build rather than a way to reuse
//! another process's work.
//!
//! # What one build retains
//!
//! Beside the executable, in the directory the build owns:
//!
//! | File               | Meaning                                              |
//! |--------------------|------------------------------------------------------|
//! | `byte_observer.rs` | the observer bytes `rustc` was given                 |
//! | `main.rs`          | the generated child bytes `rustc` was given          |
//! | `rustc-program.txt`| the one resolved compiler value, displayed and exact |
//! | `rustc-argv.txt`   | the base compilation argv sent to the identity guard |
//! | `rustc-observed-argv.json` | the complete instrumented compiler invocation |
//! | `rustc-identity.txt`| `rustc -vV` from that same value                    |
//! | `source-identity.txt`| the informational source digest and byte counts    |
//!
//! The retained sources *are* the compilation inputs — `rustc` is pointed at
//! this `main.rs`, not at a copy — so an A/B compares the executable against the
//! bytes that produced it and cannot be shown a file that drifted from them.
//! The digest is recorded so two hosts can say they built the same source; it
//! never authorises reusing anything.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const OBSERVER_FILE: &str = "byte_observer.rs";
const MAIN_FILE: &str = "main.rs";
const PROGRAM_FILE: &str = "rmux-final-sink-child.exe";
/// What the compiler writes. Renamed to [`PROGRAM_FILE`] only after the
/// compilation succeeded, so nothing observing this directory can reach a
/// half-written executable under the child's name.
const PARTIAL_PROGRAM_FILE: &str = "rmux-final-sink-child.partial.exe";
const COMPILER_PROGRAM_FILE: &str = "rustc-program.txt";
const COMPILER_ARGV_FILE: &str = "rustc-argv.txt";
const COMPILER_IDENTITY_FILE: &str = "rustc-identity.txt";
const SOURCE_IDENTITY_FILE: &str = "source-identity.txt";

/// The one visible byte sequence every child renders, aware or not, so a
/// Windows pseudoconsole has a frame to send its pending mode change in.
const RENDERED_MARKER: &str = "rmux-final-sink";

const BUILD_DIRECTORY_PREFIX: &str = "build-";
/// How many candidate directories one build will create-or-skip before giving
/// up. Only an occupied candidate costs an attempt, and in a fresh root the
/// first one always wins; the bound exists so a root that cannot be built in
/// fails attributably instead of looping.
const BUILD_DIRECTORY_ATTEMPTS: u32 = 64;

/// The observer module this test binary itself compiles.
pub(super) const OBSERVER_SOURCE: &str = include_str!("byte_observer.rs");

/// The child's entry point: both console modes, the capability announcement,
/// the readiness signal, the historical read boundary, and the park/`done`
/// teardown handshake. Everything about the capture itself is delegated to the
/// observer module, which is why nothing here re-implements a console read.
///
/// The console *output* mode is as load-bearing as the input one. Windows
/// interprets a VT sequence written to a console only when that output handle
/// carries `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; without it the announcement is
/// text the console deposits in its screen buffer, and what a pseudoconsole then
/// reproduces downstream is the host build's business rather than this child's.
/// The Unix sibling inherits nothing to do here because a pseudoterminal carries
/// its bytes verbatim, which is exactly why porting that protocol to a console
/// had to add the step instead.
pub(super) const CHILD_MAIN_SOURCE: &str = r##"#![allow(dead_code)]
//! The Windows final-sink pane child.
//!
//! Generated verbatim from
//! `crates/rmux-server/src/test_shell/final_sink/windows_byte_child.rs`; edit it
//! there. Its only read boundary is the historical probe's
//! `std::io::stdin().lock()` handed to the byte observer that the harness
//! asserting on this child compiles from the same source.

mod byte_observer;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

type Handle = isize;

#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: i32) -> Handle;
    fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
}

const STD_INPUT_HANDLE: i32 = -10;
const STD_OUTPUT_HANDLE: i32 = -11;
/// `ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT`: a cooked
/// console treats the paste's leading ESC as an editing command and rewrites
/// CR/LF, so the captured bytes would say nothing about the sink.
const COOKED_INPUT_FLAGS: u32 = 0x7;
/// `ENABLE_VIRTUAL_TERMINAL_INPUT`.
const VIRTUAL_TERMINAL_INPUT: u32 = 0x200;
/// `ENABLE_VIRTUAL_TERMINAL_PROCESSING`: without it a console takes the
/// announcement below as text for its screen buffer rather than as a sequence
/// to interpret, so this child would be leaving to the host build what it is
/// here to state itself.
const VIRTUAL_TERMINAL_OUTPUT: u32 = 0x4;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

struct Slot {
    ready: PathBuf,
    partial: PathBuf,
    out: PathBuf,
    error: PathBuf,
    stop: PathBuf,
    done: PathBuf,
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [ready, partial, out, error, stop, done, want, awareness, park] = arguments.as_slice()
    else {
        eprintln!(
            "usage: rmux-final-sink-child <ready> <out.part> <out> <error> <stop> <done> \
             <bytes> <aware|unaware> <park-seconds>"
        );
        std::process::exit(2);
    };
    let slot = Slot {
        ready: PathBuf::from(ready),
        partial: PathBuf::from(partial),
        out: PathBuf::from(out),
        error: PathBuf::from(error),
        stop: PathBuf::from(stop),
        done: PathBuf::from(done),
    };
    let (Ok(want), Ok(park)) = (want.parse::<usize>(), park.parse::<u64>()) else {
        eprintln!("the expected byte count and park duration must both be numbers");
        std::process::exit(2);
    };

    let failure = capture(&slot, want, awareness == "aware").err();
    if let Some(message) = &failure {
        // Written before the park, so the harness reports the exact reason
        // instead of waiting out a capture boundary.
        let _ = std::fs::write(&slot.error, message.as_bytes());
    }
    park_until_stopped(&slot, park);
    // `done` is the only teardown acknowledgement and is written whether or not
    // the capture succeeded: teardown is signalled separately from success.
    let _ = std::fs::write(&slot.done, b"1");
    if failure.is_some() {
        std::process::exit(1);
    }
}

fn capture(slot: &Slot, want: usize, aware: bool) -> Result<(), String> {
    // Readiness is published only after raw mode is established. A child that
    // signalled first could be read in cooked mode, which would corrupt the
    // capture without ever reporting a setup failure.
    set_raw_console_input()?;
    // Established by both children, so an aware pane and an unaware one differ
    // in the announcement itself and in nothing else about their console.
    set_virtual_terminal_output()?;
    if aware {
        announce_bracketed_paste()?;
    }
    // Also established by both children, and after the announcement: a
    // pseudoconsole carries a pending mode change only in the next frame it
    // renders.
    render_a_frame()?;
    std::fs::write(&slot.ready, b"1")
        .map_err(|error| format!("readiness could not be signalled: {error}"))?;

    let mut stdin = std::io::stdin().lock();
    byte_observer::capture_to_slot(&mut stdin, want, &slot.partial, &slot.out)
        .map(|_| ())
        .map_err(|failure| failure.to_string())
}

fn set_raw_console_input() -> Result<(), String> {
    // SAFETY: `mode` is valid writable storage for the duration of
    // `GetConsoleMode`; Windows validates the opaque standard handle, and no
    // pointer or borrowed handle escapes this block.
    let mode = unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0_u32;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return Err("GetConsoleMode failed: standard input is not a console".to_owned());
        }
        let raw = (mode & !COOKED_INPUT_FLAGS) | VIRTUAL_TERMINAL_INPUT;
        if SetConsoleMode(handle, raw) == 0 {
            return Err(format!("SetConsoleMode({raw:#x}) failed"));
        }
        raw
    };
    let _ = mode;
    Ok(())
}

/// Puts standard output into virtual-terminal processing mode.
///
/// A console does not interpret a VT sequence unless its output handle carries
/// `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; the announcement below is otherwise
/// plain text for the screen buffer, and whether a pseudoconsole reproduces
/// anything a terminal can read as a mode change stops being this child's
/// decision. Establishing it is what makes the announcement mean the same thing
/// on every Windows build.
fn set_virtual_terminal_output() -> Result<(), String> {
    // SAFETY: `mode` is valid writable storage for the duration of
    // `GetConsoleMode`; Windows validates the opaque standard handle, and no
    // pointer or borrowed handle escapes this block.
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0_u32;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return Err("GetConsoleMode failed: standard output is not a console".to_owned());
        }
        let processed = mode | VIRTUAL_TERMINAL_OUTPUT;
        if SetConsoleMode(handle, processed) == 0 {
            return Err(format!(
                "SetConsoleMode({processed:#x}) failed for standard output"
            ));
        }
    }
    Ok(())
}

/// Writes this child's own bracketed-paste capability announcement.
///
/// Checked like every other step, and like the Unix sibling's `printf`:
/// `print!` panics when the write fails, which would leave the harness a child
/// that died with no `error` to read.
fn announce_bracketed_paste() -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(b"\x1b[?2004h")
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("the capability announcement failed: {error}"))
}

/// Renders one visible marker so the pseudoconsole emits a frame at all.
///
/// A Windows pseudoconsole is a renderer, not a pipe: it sends bytes
/// downstream when the client changes what is on screen, and a client that
/// only changes a mode changes nothing on screen. Measured on this host with
/// no RMUX involved, a child whose whole output is `ESC[?2004h` leaves the
/// pty output pipe empty for as long as it is watched, while the same child
/// followed by one printable byte emits a single frame whose first bytes are
/// exactly that pending `ESC[?2004h`. A pseudoterminal carries the
/// announcement itself, which is why the Unix sibling has no step here — the
/// same reason it has no console modes to establish.
///
/// This publishes nothing about the capability. Both children render it, so a
/// child that never announced renders the identical marker and stays unaware;
/// only the announcement above decides what the frame carries.
fn render_a_frame() -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(b"rmux-final-sink")
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("the frame could not be rendered: {error}"))
}

/// Stays alive after capturing so the harness can still resolve this pane as a
/// live destination while it asserts.
fn park_until_stopped(slot: &Slot, seconds: u64) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline && !Path::new(&slot.stop).exists() {
        std::thread::sleep(POLL_INTERVAL);
    }
}
"##;

/// Everything one build of the child is identified by.
///
/// The compiler value, the argv and the identity are carried together
/// deliberately: they are the three facts that must agree, and returning only
/// a path is what let the previous build key one compiler and invoke another.
#[derive(Debug)]
struct ChildBuild {
    /// The directory this build created exclusively and owns.
    directory: PathBuf,
    /// The published executable inside it.
    program: PathBuf,
    /// Keep the selected executable immutable across later pane respawns.
    _program_guard: fs::File,
    /// The single resolved compiler value.
    compiler: OsString,
    /// The complete compilation argv, program first, exactly as invoked.
    argv: Vec<OsString>,
    /// `rustc -vV` from that same value.
    identity: String,
}

/// The compiled child, built once per process and shared by every slot.
///
/// The cache is in-process and nothing more: it remembers the outcome of *this*
/// process's own build. It is never a way to adopt an executable some other
/// process left behind.
pub(super) fn child_program() -> Result<PathBuf, String> {
    static PROGRAM: OnceLock<Result<ChildBuild, String>> = OnceLock::new();
    let build = PROGRAM
        .get_or_init(compile_child_program)
        .as_ref()
        .map_err(Clone::clone)?;
    verify_program(&build.program)?;
    Ok(build.program.clone())
}

/// A stable digest of the exact child sources.
///
/// `RandomState` is reseeded per process and `DefaultHasher` is an
/// implementation detail, so this is an explicit FNV-1a. Carriage returns are
/// excluded so a checkout's line endings cannot change the identity a later
/// Windows 10 A/B compares against.
///
/// This is evidence, never authorisation: it says two hosts compiled the same
/// bytes, and it is not consulted to decide whether an executable may be used.
pub(super) fn source_digest() -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    for byte in OBSERVER_SOURCE
        .bytes()
        .chain(CHILD_MAIN_SOURCE.bytes())
        .filter(|byte| *byte != b'\r')
    {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    }
    digest
}

fn compile_child_program() -> Result<ChildBuild, String> {
    build_child_program(&default_build_root(), compiler_program())
}

/// The container this process's build directories are created under.
///
/// Named for the process and the source digest so concurrent test binaries do
/// not contend and a differently-sourced checkout lands elsewhere. Neither is a
/// safety property: an operating system reuses process ids, so the exclusive
/// creation below is what actually keeps a build out of an occupied directory.
fn default_build_root() -> PathBuf {
    build_container().join(format!(
        "rmux-final-sink-child-{}-{:016x}",
        std::process::id(),
        source_digest()
    ))
}

/// Compiles one child into a directory this call exclusively created.
fn build_child_program(root: &Path, compiler: OsString) -> Result<ChildBuild, String> {
    let directory = fresh_build_directory(root)?;

    // The retained sources are the compilation inputs themselves, so a later
    // A/B reads the bytes that produced this executable rather than a copy of
    // them. Every write is checked: a build whose provenance was not recorded
    // is not a build this diagnostic may use.
    write_checked(&directory.join(OBSERVER_FILE), OBSERVER_SOURCE)?;
    write_checked(&directory.join(MAIN_FILE), CHILD_MAIN_SOURCE)?;
    write_checked(&directory.join(SOURCE_IDENTITY_FILE), &source_identity())?;

    // One resolved value, both uses. Asking the environment a second time is
    // how a `RUSTC` or `PATH` change records compiler A and invokes compiler B.
    let identity = compiler_identity(&compiler)?;
    write_checked(
        &directory.join(COMPILER_PROGRAM_FILE),
        &describe_program(&compiler),
    )?;
    write_checked(&directory.join(COMPILER_IDENTITY_FILE), &identity)?;

    let partial = directory.join(PARTIAL_PROGRAM_FILE);
    let argv = compilation_argv(&compiler, &directory, &partial);
    write_checked(&directory.join(COMPILER_ARGV_FILE), &describe_argv(&argv))?;

    // The existing identity compiler records the real parent and these exact
    // retained inputs before linking; it never certifies an older executable.
    let mut arguments = vec![directory.as_os_str()];
    arguments.extend(argv.iter().map(OsString::as_os_str));
    identity_action("final-sink-build", &arguments)?;
    let program = directory.join(PROGRAM_FILE);
    // Windows refuses writes/deletion while this handle is retained. The
    // cached command can be respawned later without accepting changed bytes.
    let program_guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&program)
        .map_err(|error| format!("final-sink executable cannot be protected: {error}"))?;
    verify_program(&program)?;

    Ok(ChildBuild {
        directory,
        program,
        _program_guard: program_guard,
        compiler,
        argv,
        identity,
    })
}

fn build_container() -> PathBuf {
    std::env::var_os("RMUX_IDENTITY_TARGET")
        .map(|target| PathBuf::from(target).join("fixture-inputs"))
        .unwrap_or_else(std::env::temp_dir)
}

fn identity_action(action: &str, arguments: &[&OsStr]) -> Result<(), String> {
    let python = std::env::var_os("RMUX_IDENTITY_PYTHON")
        .ok_or("final-sink child requires the candidate identity interpreter")?;
    let script = std::env::var_os("RMUX_IDENTITY_SCRIPT")
        .ok_or("final-sink child requires the candidate identity script")?;
    let mut command = Command::new(python);
    if std::env::var_os("RMUX_NATIVE_INPUT_MANIFEST").is_some() {
        command.args(["-I", "-S", "-B"]);
    }
    let result = command
        .arg(script)
        .arg("fixture")
        .arg(action)
        .args(arguments)
        .output()
        .map_err(|error| format!("final-sink identity check could not run: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "final-sink identity rejected before use: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(())
}

fn verify_program(program: &Path) -> Result<(), String> {
    identity_action("final-sink-check", &[program.as_os_str()])
}

/// The one site at which a build takes ownership of a directory.
///
/// Every candidate is created exclusively and none is ever adopted. This is the
/// correction: the previous build resolved a predictable shared path and
/// returned the executable there whenever it was a file, so a stale,
/// half-installed or planted program ran as the pinned child. An occupied
/// candidate belongs to something else — another process's build in flight, an
/// abandoned run, or someone's plant — and none of those can be told apart from
/// a path, so the next candidate is created instead.
fn fresh_build_directory(root: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|error| {
        format!(
            "the final-sink child build root {} could not be created: {error}",
            root.display()
        )
    })?;
    for attempt in 0..BUILD_DIRECTORY_ATTEMPTS {
        let directory = root.join(format!("{BUILD_DIRECTORY_PREFIX}{attempt}"));
        match fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "the final-sink child build directory {} could not be created: {error}",
                    directory.display()
                ))
            }
        }
    }
    Err(format!(
        "no free final-sink child build directory was available under {} \
         after {BUILD_DIRECTORY_ATTEMPTS} attempts",
        root.display()
    ))
}

/// The complete compilation command, program first.
fn compilation_argv(compiler: &OsStr, directory: &Path, output: &Path) -> Vec<OsString> {
    vec![
        compiler.to_owned(),
        OsString::from("--edition"),
        OsString::from("2021"),
        OsString::from("--crate-name"),
        OsString::from("rmux_final_sink_child"),
        OsString::from("--crate-type"),
        OsString::from("bin"),
        OsString::from("-o"),
        output.as_os_str().to_owned(),
        directory.join(MAIN_FILE).into_os_string(),
    ]
}

fn write_checked(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|error| {
        format!(
            "the final-sink child build could not write {}: {error}",
            path.display()
        )
    })
}

/// Cargo does not export `RUSTC` to a test binary, but the test's working
/// directory is inside the workspace, so the `rustup` shim resolves the
/// `rust-toolchain.toml` channel. An explicit `RUSTC` still wins.
///
/// Called once per build; the value it returns is what is recorded *and* what
/// is invoked.
fn compiler_program() -> OsString {
    std::env::var_os("RUSTC")
        .or_else(|| std::env::var_os("RMUX_IDENTITY_RUSTC"))
        .unwrap_or_else(|| OsString::from("rustc"))
}

/// `rustc -vV` from the value that is about to compile the child.
///
/// A compiler that cannot be run, exits non-zero or reports nothing is a hard
/// failure: an unidentified compiler is exactly what an R1 A/B cannot have, and
/// the previous build recorded that condition as a sidecar string and carried
/// on.
fn compiler_identity(compiler: &OsStr) -> Result<String, String> {
    let displayed = Path::new(compiler).display().to_string();
    let identity = Command::new(compiler)
        .arg("--version")
        .arg("--verbose")
        .output()
        .map_err(|error| {
            format!(
                "{displayed} could not be run to identify the final-sink child's compiler: {error}"
            )
        })?;
    if !identity.status.success() {
        return Err(format!(
            "{displayed} could not identify itself ({}):\n{}",
            identity.status,
            String::from_utf8_lossy(&identity.stderr)
        ));
    }
    let reported = String::from_utf8_lossy(&identity.stdout).into_owned();
    if reported.trim().is_empty() {
        return Err(format!(
            "{displayed} reported an empty compiler identity, so this build cannot be attributed"
        ));
    }
    Ok(reported)
}

/// Both forms of the one compiler value: what a person reads, and the exact
/// form, which survives a path the lossy display would mangle.
fn describe_program(compiler: &OsStr) -> String {
    format!(
        "display: {}\nexact: {compiler:?}\n",
        Path::new(compiler).display()
    )
}

fn describe_argv(argv: &[OsString]) -> String {
    let mut described = String::new();
    for (index, argument) in argv.iter().enumerate() {
        described.push_str(&format!("{index}: {argument:?}\n"));
    }
    described
}

fn source_identity() -> String {
    format!(
        "source-digest: {:016x}\n{OBSERVER_FILE}: {} bytes\n{MAIN_FILE}: {} bytes\n",
        source_digest(),
        OBSERVER_SOURCE.len(),
        CHILD_MAIN_SOURCE.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A genuine executable that is not the pinned child. Compiled rather than
    /// written as text: the rejection under test must not depend on the planted
    /// bytes being unrunnable, because a real attacker plants something that
    /// runs.
    const POISON_MAIN_SOURCE: &str = r#"fn main() {
    println!("a planted final-sink child");
}
"#;

    /// Set by the outer half of the `RUSTC` override case in the environment of
    /// the child test process that performs the overridden build.
    const OVERRIDE_CASE_ENV: &str = "RMUX_FINAL_SINK_CHILD_RUSTC_OVERRIDE_CASE";
    /// The full path of this case, so the outer half can re-run exactly it.
    const OVERRIDE_CASE_NAME: &str = "test_shell::final_sink::windows_byte_child::tests::\
         a_real_absolute_rustc_override_builds_the_child_with_that_exact_compiler";

    /// A build root no other case in this process can collide with.
    fn scratch_root(label: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = build_container().join(format!(
            "rmux-final-sink-child-case-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("create the scratch build root");
        root
    }

    /// Proves a candidate really is this harness's child rather than merely a
    /// file at the child's path: run with no arguments, the pinned child
    /// answers with its own usage and exit code 2.
    fn assert_is_the_pinned_child(program: &Path) {
        verify_program(program).unwrap_or_else(|failure| panic!("{failure}"));
        let answered = Command::new(program)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap_or_else(|error| panic!("run {}: {error}", program.display()));
        let usage = String::from_utf8_lossy(&answered.stderr).into_owned();
        assert_eq!(
            answered.status.code(),
            Some(2),
            "{} did not answer as the pinned child: {usage}",
            program.display()
        );
        assert!(
            usage.contains("usage: rmux-final-sink-child <ready>"),
            "{} did not answer as the pinned child: {usage}",
            program.display()
        );
    }

    /// Compiles [`POISON_MAIN_SOURCE`] to `output` with the same toolchain.
    fn plant_executable(output: &Path) -> Vec<u8> {
        let staging = output
            .parent()
            .expect("the planted executable has a directory")
            .join("planted-main.rs");
        fs::write(&staging, POISON_MAIN_SOURCE).expect("write the planted source");
        let compiled = Command::new(compiler_program())
            .arg("--edition")
            .arg("2021")
            .arg("--crate-name")
            .arg("planted_final_sink_child")
            .arg("-o")
            .arg(output)
            .arg(&staging)
            .output()
            .expect("run the compiler for the planted executable");
        assert!(
            compiled.status.success(),
            "the planted executable did not compile: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        fs::remove_file(&staging).expect("remove the planted source");
        fs::read(output).expect("read the planted executable")
    }

    /// The whole point of the correction: the child must read through the
    /// historical standard-input byte boundary, not a hand-written console
    /// loop. This turns red the moment either half is replaced.
    #[test]
    fn the_child_reads_through_the_historical_standard_input_byte_boundary() {
        assert!(
            CHILD_MAIN_SOURCE.contains("let mut stdin = std::io::stdin().lock();"),
            "the child must lock standard input exactly as the historical probe did"
        );
        assert!(
            CHILD_MAIN_SOURCE.contains("byte_observer::capture_to_slot(&mut stdin, want,"),
            "the locked standard input must be the observer's reader"
        );
        assert!(
            OBSERVER_SOURCE.contains("let mut buffer = [0_u8; READ_BUFFER_BYTES];"),
            "the observer must read into the historical byte buffer"
        );
        assert!(
            OBSERVER_SOURCE.contains("pub(crate) const READ_BUFFER_BYTES: usize = 4096;"),
            "the historical buffer is 4096 bytes"
        );
        assert!(
            !CHILD_MAIN_SOURCE.contains("ReadConsoleW"),
            "re-emulating the console read is the construction this correction removes"
        );
    }

    /// The child's console handling is the only thing that may not be shared
    /// with this binary, so pin the mode it establishes and the order it
    /// establishes it in.
    #[test]
    fn the_child_establishes_raw_input_before_it_signals_readiness() {
        let raw_mode = CHILD_MAIN_SOURCE
            .find("set_raw_console_input()?")
            .expect("the child establishes raw console input");
        let readiness = CHILD_MAIN_SOURCE
            .find("std::fs::write(&slot.ready")
            .expect("the child signals readiness");
        assert!(
            raw_mode < readiness,
            "readiness must never be announced before raw mode succeeds"
        );
        assert!(CHILD_MAIN_SOURCE.contains("const COOKED_INPUT_FLAGS: u32 = 0x7;"));
        assert!(CHILD_MAIN_SOURCE.contains("const VIRTUAL_TERMINAL_INPUT: u32 = 0x200;"));
    }

    /// The finding this correction is for. A console interprets a VT sequence
    /// only when its output handle carries
    /// `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; a child that announces
    /// `ESC[?2004h` without it has handed the meaning of its own announcement
    /// to whatever the host build does with text in a screen buffer. The child
    /// established its input mode and never its output mode, which is the step
    /// the Unix sibling has no need of and the port therefore never gained.
    ///
    /// Both children establish it, so the aware proof and the unaware negative
    /// control differ in the announcement and in nothing else.
    #[test]
    fn the_child_enables_virtual_terminal_output_before_it_announces_the_capability() {
        assert!(
            CHILD_MAIN_SOURCE.contains("const STD_OUTPUT_HANDLE: i32 = -11;"),
            "the child must address its own standard output"
        );
        assert!(
            CHILD_MAIN_SOURCE.contains("const VIRTUAL_TERMINAL_OUTPUT: u32 = 0x4;"),
            "the child must name ENABLE_VIRTUAL_TERMINAL_PROCESSING"
        );

        let at = |needle: &str| {
            CHILD_MAIN_SOURCE
                .find(needle)
                .unwrap_or_else(|| panic!("the child must run {needle:?}"))
        };
        let raw_input = at("set_raw_console_input()?");
        let virtual_terminal_output = at("set_virtual_terminal_output()?");
        let awareness = at("if aware {");
        let announcement = at("announce_bracketed_paste()?");
        let readiness = at("std::fs::write(&slot.ready");

        assert!(
            raw_input < virtual_terminal_output,
            "the console modes are established together, input first"
        );
        assert!(
            virtual_terminal_output < awareness,
            "an unaware child must establish exactly the console an aware one does"
        );
        assert!(
            awareness < announcement,
            "only an aware child announces the capability"
        );
        assert!(
            announcement < readiness,
            "readiness must never be signalled before the announcement it precedes"
        );
    }

    /// The finding the eight announcement-waiting proofs were red for. A
    /// pseudoconsole forwards a mode change only inside the next frame it
    /// renders, and a child whose entire output is `ESC[?2004h` renders none:
    /// natively measured, its pty output pipe stays empty, which is exactly
    /// the "applied no output at all" boundary those proofs reported.
    ///
    /// The marker is rendered by both children and after the announcement, so
    /// it forces the frame without deciding what that frame carries.
    #[test]
    fn the_child_renders_a_frame_after_announcing_and_before_signalling_readiness() {
        let at = |needle: &str| {
            CHILD_MAIN_SOURCE
                .find(needle)
                .unwrap_or_else(|| panic!("the child must run {needle:?}"))
        };
        let announcement = at("announce_bracketed_paste()?");
        let render = at("render_a_frame()?");
        let readiness = at("std::fs::write(&slot.ready");

        assert!(
            announcement < render,
            "a frame rendered before the announcement could not carry it"
        );
        assert!(
            render < readiness,
            "the frame must be rendered before the harness is told to look for the mode"
        );
        assert!(
            CHILD_MAIN_SOURCE.contains(&format!(".write_all(b\"{RENDERED_MARKER}\")")),
            "the child must write the visible marker itself"
        );
        assert!(
            !CHILD_MAIN_SOURCE.contains("if aware {\n        render_a_frame"),
            "an unaware child must render exactly what an aware one renders"
        );
    }

    /// The announcement is this child's own capability claim, so a write it
    /// could not perform is owed an `error` like every other step — the Unix
    /// sibling has always guarded its `printf`. `print!` panicked instead,
    /// leaving the harness a dead child and nothing to read.
    #[test]
    fn an_announcement_the_child_could_not_write_is_reported_rather_than_panicked() {
        assert!(
            !CHILD_MAIN_SOURCE.contains("print!("),
            "a panicking write leaves no attributable failure behind"
        );
        assert!(
            CHILD_MAIN_SOURCE.contains(r#".write_all(b"\x1b[?2004h")"#),
            "the child must write the announcement itself, checked"
        );
        assert!(
            CHILD_MAIN_SOURCE.contains("the capability announcement failed"),
            "a failed announcement must name itself"
        );
    }

    /// A digest that is not stable is not identity. This also fails loudly if
    /// the sources are ever loaded through a path that rewrites line endings.
    #[test]
    fn the_child_source_digest_is_stable_within_a_run() {
        assert_eq!(source_digest(), source_digest());
        assert!(!OBSERVER_SOURCE.contains('\r'));
        assert!(!CHILD_MAIN_SOURCE.contains('\r'));
    }

    /// A directly generated Windows executable must retain the same candidate
    /// identity and transitive build graph as its actual Rust test producer.
    /// Its graph is bound to the Cargo lock contexts it was built under, so the
    /// identity harness writes the locked `V2` graph marker; `V1` is the
    /// unlocked form and would mean the child escaped its lock binding.
    #[test]
    fn the_generated_final_sink_child_has_candidate_identity_before_use() {
        let root = scratch_root("candidate-identity");
        let build = build_child_program(&root, compiler_program())
            .unwrap_or_else(|failure| panic!("{failure}"));
        let bytes = fs::read(&build.program).expect("compiled Windows child");
        for marker in [
            b"RMUX_BUILD_IDENTITY_V1:".as_slice(),
            b"RMUX_BUILD_GRAPH_V2:",
        ] {
            assert!(
                bytes.windows(marker.len()).any(|part| part == marker),
                "generated final-sink child lacks {} before use: {}",
                String::from_utf8_lossy(marker),
                build.program.display()
            );
        }
        assert_is_the_pinned_child(&build.program);
    }

    /// The pinned source must actually build with the workspace toolchain, and
    /// the build must be able to say what it built: these are the links the R1
    /// A/B follows from a committed source byte to an executed one.
    #[test]
    fn a_fresh_private_build_retains_the_sources_compiler_and_executable_it_used() {
        let root = scratch_root("provenance");
        let build = build_child_program(&root, compiler_program())
            .unwrap_or_else(|failure| panic!("{failure}"));
        eprintln!(
            "final-sink child build: directory={} program={}",
            build.directory.display(),
            build.program.display()
        );

        assert_eq!(
            build.directory,
            root.join(format!("{BUILD_DIRECTORY_PREFIX}0")),
            "a fresh root's first candidate is the one that is created"
        );
        assert_eq!(build.program, build.directory.join(PROGRAM_FILE));
        assert!(build.program.is_file(), "the child was not published");
        assert!(
            !build.directory.join(PARTIAL_PROGRAM_FILE).exists(),
            "publication renames the partial executable rather than copying it"
        );

        // The retained sources are the compilation inputs, not copies of them.
        assert_eq!(
            fs::read_to_string(build.directory.join(OBSERVER_FILE)).expect("retained observer"),
            OBSERVER_SOURCE
        );
        assert_eq!(
            fs::read_to_string(build.directory.join(MAIN_FILE)).expect("retained main"),
            CHILD_MAIN_SOURCE
        );
        assert_eq!(
            build.argv.last().expect("the argv names a source"),
            build.directory.join(MAIN_FILE).as_os_str(),
            "the compiler must have been pointed at the retained source"
        );
        assert_eq!(
            fs::read_to_string(build.directory.join(SOURCE_IDENTITY_FILE))
                .expect("retained source identity"),
            source_identity()
        );

        // One compiler value produced both the identity and the executable.
        assert_eq!(
            build.argv.first().expect("the argv names a compiler"),
            &build.compiler,
            "the recorded compiler must be the one that was invoked"
        );
        assert_eq!(
            fs::read_to_string(build.directory.join(COMPILER_PROGRAM_FILE))
                .expect("retained compiler"),
            describe_program(&build.compiler)
        );
        assert_eq!(
            fs::read_to_string(build.directory.join(COMPILER_ARGV_FILE)).expect("retained argv"),
            describe_argv(&build.argv)
        );
        let identity =
            fs::read_to_string(build.directory.join(COMPILER_IDENTITY_FILE)).expect("retained -vV");
        assert_eq!(identity, build.identity);
        assert!(
            identity.starts_with("rustc ") && identity.contains("\nhost: "),
            "the retained identity must be a real `rustc -vV`: {identity}"
        );

        assert_is_the_pinned_child(&build.program);
    }

    /// The finding itself, against the production selector rather than a
    /// helper: an executable planted at the path a build would use must never
    /// become this harness's child.
    ///
    /// The plant is a genuine compiled program, so nothing here turns on the
    /// bytes being unrunnable — only on the build refusing to adopt what it did
    /// not create.
    #[test]
    fn a_pre_populated_build_target_is_never_selected_as_the_child() {
        let root = scratch_root("planted-target");
        let occupied = root.join(format!("{BUILD_DIRECTORY_PREFIX}0"));
        fs::create_dir_all(&occupied).expect("stage the occupied build target");
        let planted_path = occupied.join(PROGRAM_FILE);
        let planted = plant_executable(&planted_path);

        let build = build_child_program(&root, compiler_program())
            .unwrap_or_else(|failure| panic!("{failure}"));

        assert_ne!(
            build.directory, occupied,
            "an occupied build target must never be adopted"
        );
        assert_eq!(
            build.directory,
            root.join(format!("{BUILD_DIRECTORY_PREFIX}1")),
            "the build must move to a distinct freshly created directory"
        );
        assert_ne!(
            build.program, planted_path,
            "the planted path must never be the selected child"
        );
        assert_ne!(
            fs::read(&build.program).expect("the selected child"),
            planted,
            "the planted executable must never be the selected child's bytes"
        );
        assert_eq!(
            fs::read(&planted_path).expect("the planted executable survives"),
            planted,
            "refusing a build target must not destroy the evidence in it"
        );
        assert_is_the_pinned_child(&build.program);
    }

    /// A root in which no candidate can be created fails attributably instead
    /// of adopting one or looping.
    #[test]
    fn a_root_whose_candidates_are_all_occupied_fails_attributably() {
        let root = scratch_root("exhausted");
        for attempt in 0..BUILD_DIRECTORY_ATTEMPTS {
            fs::create_dir(root.join(format!("{BUILD_DIRECTORY_PREFIX}{attempt}")))
                .expect("occupy a candidate");
        }

        let failure = build_child_program(&root, compiler_program())
            .expect_err("an exhausted root must not produce a child");

        assert!(
            failure.contains("no free final-sink child build directory was available"),
            "unexpected failure: {failure}"
        );
        assert!(
            failure.contains(&root.display().to_string()),
            "the failure must name the root it gave up on: {failure}"
        );
    }

    /// A compiler that cannot identify itself must stop the build rather than
    /// produce a child no A/B can attribute.
    #[test]
    fn a_compiler_that_cannot_identify_itself_stops_the_build() {
        let root = scratch_root("unidentifiable");

        let failure = build_child_program(
            &root,
            OsString::from("rmux-final-sink-child-no-such-compiler"),
        )
        .expect_err("an unidentifiable compiler must not produce a child");

        assert!(
            failure.contains("could not be run to identify"),
            "unexpected failure: {failure}"
        );
        assert!(
            !root
                .join(format!("{BUILD_DIRECTORY_PREFIX}0"))
                .join(PROGRAM_FILE)
                .exists(),
            "a build that could not identify its compiler must publish nothing"
        );
    }

    /// A real build driven by an explicit absolute `RUSTC`.
    ///
    /// Two halves of one case. The outer half resolves the toolchain's own
    /// `rustc.exe`, then re-runs *this exact case* in a child test process with
    /// `RUSTC` set to that absolute path; the inner half performs a genuine
    /// build there and prints what it used. Nothing mutates this process's
    /// environment, so no parallel case can observe a half-applied override,
    /// and the override is exercised through `compiler_program` itself rather
    /// than through a value handed to a helper.
    #[test]
    fn a_real_absolute_rustc_override_builds_the_child_with_that_exact_compiler() {
        if std::env::var_os(OVERRIDE_CASE_ENV).is_some() {
            perform_the_overridden_build();
            return;
        }

        let absolute = toolchain_rustc_executable();
        assert!(
            absolute.is_absolute() && absolute.is_file(),
            "{} is not an absolute compiler to override with",
            absolute.display()
        );

        let parent = std::env::current_exe().expect("this test binary");
        verify_program(&parent).unwrap_or_else(|failure| panic!("{failure}"));
        let inner = Command::new(parent)
            .args([
                OVERRIDE_CASE_NAME,
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(OVERRIDE_CASE_ENV, "1")
            .env("RUSTC", &absolute)
            .output()
            .expect("re-run this case with an absolute RUSTC");
        let reported = format!(
            "{}{}",
            String::from_utf8_lossy(&inner.stdout),
            String::from_utf8_lossy(&inner.stderr)
        );
        assert!(
            inner.status.success(),
            "the overridden build failed:\n{reported}"
        );
        assert!(
            reported.contains(&format!(
                "overridden-build compiler: {}",
                absolute.display()
            )),
            "the overridden build must have used the absolute compiler:\n{reported}"
        );
        assert!(
            reported.contains(&format!("overridden-build argv0: {}", absolute.display())),
            "the compilation must have been invoked through it:\n{reported}"
        );
        assert!(
            reported.contains("overridden-build identity: rustc "),
            "the identity must have come from it:\n{reported}"
        );
    }

    /// The inner half: a genuine build whose compiler came from `RUSTC`.
    fn perform_the_overridden_build() {
        let root = scratch_root("rustc-override");
        let compiler = compiler_program();
        assert!(
            Path::new(&compiler).is_absolute(),
            "the overridden case must receive an absolute RUSTC: {compiler:?}"
        );

        let build =
            build_child_program(&root, compiler).unwrap_or_else(|failure| panic!("{failure}"));

        println!(
            "overridden-build compiler: {}",
            Path::new(&build.compiler).display()
        );
        println!(
            "overridden-build argv0: {}",
            Path::new(build.argv.first().expect("argv0")).display()
        );
        println!(
            "overridden-build identity: {}",
            build.identity.lines().next().unwrap_or_default()
        );
        println!("overridden-build program: {}", build.program.display());
        assert_is_the_pinned_child(&build.program);
    }

    /// The toolchain's own `rustc.exe`, resolved through the compiler that is
    /// in use rather than through `PATH`.
    fn toolchain_rustc_executable() -> PathBuf {
        let sysroot = Command::new(compiler_program())
            .arg("--print")
            .arg("sysroot")
            .output()
            .expect("ask the compiler for its sysroot");
        assert!(sysroot.status.success(), "the compiler has a sysroot");
        PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim().to_owned())
            .join("bin")
            .join("rustc.exe")
    }

    /// The child's own failure path: a standard input that is not a console
    /// must be reported through `error`, must not publish `out`, and must still
    /// acknowledge teardown through `done`.
    #[test]
    fn the_child_reports_a_setup_failure_and_still_acknowledges_teardown() {
        let program = child_program().unwrap_or_else(|failure| panic!("{failure}"));
        let directory = std::env::temp_dir().join(format!(
            "rmux-final-sink-child-setup-{}-{:016x}",
            std::process::id(),
            source_digest()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create the scratch slot");
        let path = |file: &str| directory.join(file).display().to_string();
        // Pre-signalled, so the child leaves its park immediately.
        fs::write(directory.join("stop"), b"1").expect("stage the stop signal");

        let status = Command::new(&program)
            .args([
                path("ready"),
                path("out.part"),
                path("out"),
                path("error"),
                path("stop"),
                path("done"),
                "16".to_owned(),
                "aware".to_owned(),
                "5".to_owned(),
            ])
            // Never inherit this process's console: the child would put the
            // test runner's own input into raw mode.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run the compiled final-sink child");

        let reported = fs::read_to_string(directory.join("error")).unwrap_or_default();
        assert!(
            reported.contains("GetConsoleMode failed"),
            "the child must attribute its setup failure: {reported:?}"
        );
        assert!(
            !directory.join("ready").exists(),
            "readiness must not be signalled when raw mode was never established"
        );
        assert!(
            !directory.join("out").exists(),
            "a failed child must never publish a capture"
        );
        assert!(
            directory.join("done").is_file(),
            "teardown must be acknowledged even when the capture failed"
        );
        assert_eq!(
            status.code(),
            Some(1),
            "a failed capture must exit non-zero"
        );
        let _ = fs::remove_dir_all(&directory);
    }
}
