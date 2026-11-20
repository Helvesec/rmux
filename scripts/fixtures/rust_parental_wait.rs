//! Minimal native acceptance includes the actual public consumer boundary.
#[allow(dead_code)]
#[path = "../../tests/support/public_api_compile_fail.rs"]
mod consumer;

#[used]
static CANDIDATE_HEAD: &str = env!("RMUX_F7_HEAD");

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("child") {
        print!("stdout:{}", args[2]);
        eprint!("stderr:{}", args[2]);
        std::process::exit(args[2].parse().unwrap());
    }
    let mut command = std::process::Command::new(&args[1]);
    command.args(["child", &args[2]]);
    match consumer::consumer_output(&mut command, &args[3]) {
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
