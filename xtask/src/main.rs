#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![forbid(unsafe_code)]

//! Repository maintenance command entry point.
//!
//! The xtask binary owns repository-wide automation that does not belong
//! inside the published `rmux` binary. The current surface is:
//!
//! - `help` &mdash; print top-level help and exit.
//! - `feature-inventory` &mdash; regenerate the v1 public public overview files from their
//!   canonical sources in [`feature_inventory`].
//! - `feature-inventory --check` &mdash; fail if any generated public overview drifts from
//!   the canonical source. Idempotent: running `feature-inventory` then
//!   `feature-inventory --check` must always succeed.
//! - `feature-inventory` &mdash; validate and render the public feature
//!   inventory used by the v0.0.5/v1 readiness gates.

use std::env;
use std::process::ExitCode;

mod feature_inventory;
mod feature_inventory;

const HELP: &str = "\
RMUX repository tasks

Usage:
    cargo run -p xtask -- <command> [args]

Commands:
    --help, -h, help            Print this help text.
    feature-inventory                      Regenerate v1 public overview files from xtask/assets/.
    feature-inventory --check              Fail if any generated public overview drifts from canon.
    feature-inventory --check
                                Validate docs/feature-inventory-v1.yaml.
    feature-inventory --check-file-sizes
                                Check tracked rmux-sdk source-size exceptions.
    feature-inventory --render-markdown
                                Render the YAML inventory as a Markdown table.
";

fn main() -> ExitCode {
    match parse_args(env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Command::Feature inventory { check }) => run_feature_inventory(check),
        Ok(Command::FeatureInventory { mode }) => run_feature_inventory(mode),
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            eprint!("{HELP}");
            ExitCode::from(2)
        }
    }
}

fn run_feature_inventory(check: bool) -> ExitCode {
    let mode = if check {
        feature_inventory::Mode::Check
    } else {
        feature_inventory::Mode::Write
    };
    let repo_root = feature_inventory::repo_root_from_manifest_dir();
    match feature_inventory::run(mode, &repo_root) {
        Ok(report) => {
            for path in &report.changed {
                println!("feature-inventory: wrote {}", path.display());
            }
            for path in &report.unchanged {
                println!("feature-inventory: unchanged {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("xtask feature-inventory failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_feature_inventory(mode: feature_inventory::Mode) -> ExitCode {
    let repo_root = feature_inventory::repo_root_from_manifest_dir();
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
    Feature inventory { check: bool },
    FeatureInventory { mode: feature_inventory::Mode },
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
            let mut check = false;
            for extra in args {
                match extra.as_str() {
                    "--check" => check = true,
                    other => {
                        return Err(format!("unknown feature-inventory argument: {other}"));
                    }
                }
            }
            Ok(Command::Feature inventory { check })
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
        other => Err(format!("unknown xtask command: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{feature_inventory, parse_args, Command};

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
    fn feature_inventory_defaults_to_write_mode() {
        assert_eq!(parse_args(["feature-inventory"]), Ok(Command::Feature inventory { check: false }));
    }

    #[test]
    fn feature_inventory_check_flag_selects_check_mode() {
        assert_eq!(
            parse_args(["feature-inventory", "--check"]),
            Ok(Command::Feature inventory { check: true })
        );
    }

    #[test]
    fn feature_inventory_rejects_unknown_argument() {
        assert_eq!(
            parse_args(["feature-inventory", "--bogus"]).expect_err("unknown feature-inventory flag errors"),
            "unknown feature-inventory argument: --bogus"
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
}
