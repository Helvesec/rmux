use std::fmt;
use std::path::{Path, PathBuf};

#[cfg(not(test))]
use clap_complete::generate_to;
#[cfg(not(test))]
use clap_complete::shells::{Bash, Elvish, Fish, PowerShell, Zsh};

#[cfg(not(test))]
use crate::cli_args;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shell {
    Bash,
    Zsh,
    Fish,
    Pwsh,
    Elvish,
}

impl Shell {
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) const ALL: [Self; 5] = [Self::Bash, Self::Zsh, Self::Fish, Self::Pwsh, Self::Elvish];

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "fish" => Ok(Self::Fish),
            "powershell" | "pwsh" => Ok(Self::Pwsh),
            "elvish" => Ok(Self::Elvish),
            _ => Err(format!(
                "unknown completion shell: {value}; expected one of bash, zsh, fish, powershell, elvish"
            )),
        }
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Pwsh => "powershell",
            Self::Elvish => "elvish",
        };
        formatter.write_str(name)
    }
}

#[cfg(not(test))]
pub(crate) fn generate(output_dir: &Path, shells: &[Shell]) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(output_dir).map_err(|error| {
        format!(
            "failed to create completion output directory {}: {error}",
            output_dir.display()
        )
    })?;

    let requested = if shells.is_empty() {
        &Shell::ALL
    } else {
        shells
    };
    let mut generated = Vec::with_capacity(requested.len());
    for shell in requested {
        let mut command = cli_args::completion_command();
        let path = match shell {
            Shell::Bash => generate_to(Bash, &mut command, "rmux", output_dir),
            Shell::Zsh => generate_to(Zsh, &mut command, "rmux", output_dir),
            Shell::Fish => generate_to(Fish, &mut command, "rmux", output_dir),
            Shell::Pwsh => generate_to(PowerShell, &mut command, "rmux", output_dir),
            Shell::Elvish => generate_to(Elvish, &mut command, "rmux", output_dir),
        }
        .map_err(|error| format!("failed to generate {shell} completions: {error}"))?;
        generated.push(path);
    }

    Ok(generated)
}

#[cfg(test)]
pub(crate) fn generate(_output_dir: &Path, _shells: &[Shell]) -> Result<Vec<PathBuf>, String> {
    Err("completion generation is covered by cargo check for the xtask binary".to_owned())
}
