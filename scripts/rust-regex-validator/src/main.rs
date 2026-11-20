use std::io::{self, Read};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut pattern = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut pattern) {
        eprintln!("cannot read regex: {error}");
        return ExitCode::from(2);
    }
    // The tunnel preset loader uses this exact string API with default options.
    match regex::Regex::new(&pattern) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}
