#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![forbid(unsafe_code)]

//! Repository maintenance command entry point.
//!
//! The xtask binary owns repository-wide automation that does not belong
//! inside the published `rmux` binary. The current surface is:
//!
//! - `help` &mdash; print top-level help and exit.
//! - `feature-inventory` &mdash; validate and render the tracked feature
//!   inventory.
//! - `generate-completions` &mdash; generate shell completion assets from the
//!   synthetic RMUX command surface.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(not(test))]
#[allow(missing_docs, unused)]
#[path = "../../src/cli_args.rs"]
mod cli_args;
mod completions;
mod feature_inventory;

const HELP: &str = "\
RMUX repository tasks

Usage:
    cargo run -p xtask -- <command> [args]

Commands:
    --help, -h, help            Print this help text.
    feature-inventory --check
                                Validate spec/feature-inventory-v1.yaml.
    feature-inventory --check-file-sizes
                                Check tracked rmux-sdk source-size exceptions.
    feature-inventory --render-markdown
                                Render the YAML inventory as a Markdown table.
    generate-completions --output-dir <path> [--shell <name>]...
                                Generate bash, zsh, fish, PowerShell, and
                                Elvish completion files. Use --shell to limit
                                the generated set.";

fn main() -> ExitCode {
    match parse_args(env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Command::FeatureInventory { mode }) => run_feature_inventory(mode),
        Ok(Command::GenerateCompletions { output_dir, shells }) => {
            run_generate_completions(&output_dir, &shells)
        }
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            eprint!("{HELP}");
            ExitCode::from(2)
        }
    }
}

fn run_generate_completions(
    output_dir: &std::path::Path,
    shells: &[completions::Shell],
) -> ExitCode {
    match completions::generate(output_dir, shells) {
        Ok(paths) => {
            for path in paths {
                println!("{}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("xtask generate-completions failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_feature_inventory(mode: feature_inventory::Mode) -> ExitCode {
    let repo_root = repo_root_from_manifest_dir();
    match feature_inventory::run(mode, &repo_root) {
        Ok(Some(markdown)) => {
            print!("{markdown}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask feature-inventory failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    FeatureInventory {
        mode: feature_inventory::Mode,
    },
    GenerateCompletions {
        output_dir: PathBuf,
        shells: Vec<completions::Shell>,
    },
}

fn repo_root_from_manifest_dir() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or(manifest_dir)
}

fn parse_args<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let Some(command) = args.next() else {
        return Ok(Command::Help);
    };

    match command.as_str() {
        "--help" | "-h" | "help" => {
            if let Some(extra) = args.next() {
                return Err(format!(
                    "unexpected xtask argument after {command}: {extra}"
                ));
            }
            Ok(Command::Help)
        }
        "feature-inventory" => {
            let mut mode = None;
            for extra in args {
                let next_mode = match extra.as_str() {
                    "--check" => feature_inventory::Mode::Check,
                    "--check-file-sizes" => feature_inventory::Mode::CheckFileSizes,
                    "--render-markdown" => feature_inventory::Mode::RenderMarkdown,
                    other => {
                        return Err(format!("unknown feature-inventory argument: {other}"));
                    }
                };
                if mode.replace(next_mode).is_some() {
                    return Err("feature-inventory accepts exactly one mode flag".to_owned());
                }
            }
            Ok(Command::FeatureInventory {
                mode: mode.ok_or_else(|| {
                    "feature-inventory requires one of --check, --check-file-sizes, or --render-markdown"
                        .to_owned()
                })?,
            })
        }
        "generate-completions" => parse_generate_completions_args(args),
        other => Err(format!("unknown xtask command: {other}")),
    }
}

fn parse_generate_completions_args<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mut output_dir = None;
    let mut shells = Vec::new();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--output-dir" => {
                let value = args.next().ok_or_else(|| {
                    "generate-completions --output-dir requires a value".to_owned()
                })?;
                output_dir = Some(PathBuf::from(value));
            }
            "--shell" => {
                let value = args
                    .next()
                    .ok_or_else(|| "generate-completions --shell requires a value".to_owned())?;
                shells.push(completions::Shell::parse(&value)?);
            }
            other => return Err(format!("unknown generate-completions argument: {other}")),
        }
    }

    Ok(Command::GenerateCompletions {
        output_dir: output_dir
            .ok_or_else(|| "generate-completions requires --output-dir <path>".to_owned())?,
        shells,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{completions, feature_inventory, parse_args, Command};

    #[test]
    fn no_args_prints_help() {
        assert_eq!(parse_args([] as [&str; 0]), Ok(Command::Help));
    }

    #[test]
    fn help_aliases_print_help() {
        for alias in ["--help", "-h", "help"] {
            assert_eq!(parse_args([alias]), Ok(Command::Help));
        }
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert_eq!(
            parse_args(["build"]).expect_err("unknown command errors"),
            "unknown xtask command: build"
        );
    }

    #[test]
    fn extra_argument_to_help_is_an_error() {
        assert_eq!(
            parse_args(["--help", "build"]).expect_err("extra argument errors"),
            "unexpected xtask argument after --help: build"
        );
    }

    #[test]
    fn feature_inventory_check_flag_selects_check_mode() {
        assert_eq!(
            parse_args(["feature-inventory", "--check"]),
            Ok(Command::FeatureInventory {
                mode: feature_inventory::Mode::Check
            })
        );
    }

    #[test]
    fn feature_inventory_file_size_flag_selects_file_size_mode() {
        assert_eq!(
            parse_args(["feature-inventory", "--check-file-sizes"]),
            Ok(Command::FeatureInventory {
                mode: feature_inventory::Mode::CheckFileSizes
            })
        );
    }

    #[test]
    fn feature_inventory_render_flag_selects_markdown_mode() {
        assert_eq!(
            parse_args(["feature-inventory", "--render-markdown"]),
            Ok(Command::FeatureInventory {
                mode: feature_inventory::Mode::RenderMarkdown
            })
        );
    }

    #[test]
    fn feature_inventory_requires_one_mode() {
        assert_eq!(
            parse_args(["feature-inventory"]).expect_err("missing mode errors"),
            "feature-inventory requires one of --check, --check-file-sizes, or --render-markdown"
        );
    }

    #[test]
    fn feature_inventory_rejects_multiple_modes() {
        assert_eq!(
            parse_args(["feature-inventory", "--check", "--render-markdown"])
                .expect_err("duplicate mode errors"),
            "feature-inventory accepts exactly one mode flag"
        );
    }

    #[test]
    fn generate_completions_requires_output_dir() {
        assert_eq!(
            parse_args(["generate-completions"]).expect_err("missing output dir errors"),
            "generate-completions requires --output-dir <path>"
        );
    }

    #[test]
    fn generate_completions_accepts_shell_filters() {
        assert_eq!(
            parse_args([
                "generate-completions",
                "--output-dir",
                "target/completions",
                "--shell",
                "bash",
                "--shell",
                "zsh",
            ]),
            Ok(Command::GenerateCompletions {
                output_dir: PathBuf::from("target/completions"),
                shells: vec![completions::Shell::Bash, completions::Shell::Zsh],
            })
        );
    }

    #[test]
    fn generate_completions_rejects_unknown_shell() {
        assert_eq!(
            parse_args([
                "generate-completions",
                "--output-dir",
                "target/completions",
                "--shell",
                "tcsh",
            ])
            .expect_err("unknown shell errors"),
            "unknown completion shell: tcsh; expected one of bash, zsh, fish, powershell, elvish"
        );
    }
}
