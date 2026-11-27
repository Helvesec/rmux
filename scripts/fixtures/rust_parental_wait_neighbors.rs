#[allow(dead_code)]
#[path = "../../tests/support/public_api_compile_fail.rs"]
mod consumer;

fn main() {
    std::hint::black_box(env!("RMUX_F7_HEAD"));
    let args: Vec<String> = std::env::args().collect();
    if args[1] == "child" {
        use std::io::Write;
        if let Some(path) = std::env::var_os("RMUX_F7_MARKER") {
            std::fs::write(path, b"child actually executed").unwrap();
        }
        let size = std::env::var("RMUX_F7_BYTES").ok().map(|s| s.parse().unwrap()).unwrap_or(0);
        std::io::stdout().write_all(&vec![b'o'; size]).unwrap();
        std::io::stderr().write_all(&vec![b'e'; size]).unwrap();
        std::process::exit(args[2].parse().unwrap());
    }
    let mut command = std::process::Command::new(&args[1]);
    command.args(["child", &args[2]]);
    match args.get(3).map(String::as_str) {
        Some("drop-manifest") => { command.env_remove("RMUX_NATIVE_INPUT_MANIFEST"); }
        Some("drop-digest") => { command.env_remove("RMUX_NATIVE_INPUT_MANIFEST_SHA256"); }
        Some("adverse-head") => { command.env("RMUX_IDENTITY_HEAD", "f".repeat(40)); }
        Some("adverse-session") => { command.env("RMUX_IDENTITY_DOC_SESSION", "not-a-session"); }
        _ => {}
    }
    match consumer::consumer_output(&mut command, "cargo-consumer") {
        Ok(output) => {
            use std::io::Write;
            std::io::stdout().write_all(&output.stdout).unwrap();
            std::io::stderr().write_all(&output.stderr).unwrap();
            std::process::exit(output.status.code().unwrap_or(99));
        }
        Err(error) => {
            eprintln!("launch-error:{error}");
            std::process::exit(86);
        }
    }
}
