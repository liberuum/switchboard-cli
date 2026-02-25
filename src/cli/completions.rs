use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory};
use clap_complete::{Shell, generate};
use colored::Colorize;
use std::io;
use std::path::PathBuf;

use crate::cli::Cli;

#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for (auto-detected from $SHELL if omitted)
    pub shell: Option<Shell>,

    /// Install completions into your shell config file
    #[arg(long)]
    pub install: bool,
}

// ── Shell detection ────────────────────────────────────────────────────────

fn detect_shell() -> Result<Shell> {
    let shell_env = std::env::var("SHELL").unwrap_or_default();
    if shell_env.contains("zsh") {
        Ok(Shell::Zsh)
    } else if shell_env.contains("bash") {
        Ok(Shell::Bash)
    } else if shell_env.contains("fish") {
        Ok(Shell::Fish)
    } else {
        bail!(
            "Could not detect your shell from $SHELL ({shell_env}).\n\
             Specify it explicitly: switchboard completions bash|zsh|fish"
        )
    }
}

fn rc_file(shell: Shell) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    match shell {
        Shell::Bash => Ok(home.join(".bashrc")),
        Shell::Zsh => Ok(home.join(".zshrc")),
        Shell::Fish => Ok(home
            .join(".config/fish/completions")
            .join("switchboard.fish")),
        _ => bail!("Auto-install is not supported for {shell:?}. Add completions manually."),
    }
}

fn eval_line(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => r#"eval "$(switchboard completions bash)""#,
        Shell::Zsh => r#"eval "$(switchboard completions zsh)""#,
        _ => unreachable!(),
    }
}

// ── Entry point ────────────────────────────────────────────────────────────

pub fn run(args: CompletionsArgs) -> Result<()> {
    let shell = match args.shell {
        Some(s) => s,
        None => detect_shell()?,
    };

    if args.install {
        install_completions(shell)
    } else {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        generate(shell, &mut cmd, name, &mut io::stdout());
        Ok(())
    }
}

// ── Installer ──────────────────────────────────────────────────────────────

fn install_completions(shell: Shell) -> Result<()> {
    let target = rc_file(shell)?;

    match shell {
        Shell::Fish => {
            // Fish: write completions directly to the completions directory
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::new();
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut buf);
            std::fs::write(&target, &buf)?;
            println!("{} Wrote completions to {}", "✓".green(), target.display());
        }
        Shell::Bash | Shell::Zsh => {
            let line = eval_line(shell);

            // Check if already installed
            if target.exists() {
                let contents = std::fs::read_to_string(&target)?;
                if contents.contains("switchboard completions") {
                    println!(
                        "{} Completions already configured in {}",
                        "✓".green(),
                        target.display()
                    );
                    return Ok(());
                }
            }

            // Append eval line to rc file
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&target)?;
            writeln!(file)?;
            writeln!(file, "# Switchboard CLI completions")?;
            writeln!(file, "{line}")?;

            println!("{} Added completions to {}", "✓".green(), target.display());
            println!(
                "  Restart your shell or run: {}",
                format!("source {}", target.display()).dimmed()
            );
        }
        _ => bail!("Auto-install is not supported for {shell:?}. Add completions manually."),
    }

    Ok(())
}
