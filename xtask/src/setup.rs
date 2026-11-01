//! `cargo xtask setup`: checks the host tools.

use crate::tools::{Distro, Status, TOOLS, Tool};
use anyhow::{Context, Result, bail};
use std::io::{self, BufRead, Write};
use std::process::Command;

#[derive(clap::Args)]
pub struct Args {
    /// Offer to install missing tools, asking before each command.
    #[arg(long)]
    install: bool,
}

pub fn run(args: &Args) -> Result<()> {
    let mut missing: Vec<&Tool> = Vec::new();
    for tool in &TOOLS {
        match tool.check() {
            Status::Found(v) => println!("ok       {} {v}", tool.name),
            Status::TooOld(v) => {
                println!(
                    "too old  {} {v}, need {} or newer",
                    tool.name, tool.min_version
                );
                missing.push(tool);
            }
            Status::Missing => {
                println!(
                    "missing  {} ({}), need {} or newer",
                    tool.name, tool.program, tool.min_version
                );
                missing.push(tool);
            }
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    if args.install {
        install(&missing)?;
    } else {
        print_install_commands(&missing);
    }

    let still_missing: Vec<&str> = missing
        .iter()
        .filter(|tool| !matches!(tool.check(), Status::Found(_)))
        .map(|tool| tool.name)
        .collect();
    if !still_missing.is_empty() {
        bail!("missing host tools: {}", still_missing.join(", "));
    }
    Ok(())
}

fn print_install_commands(missing: &[&Tool]) {
    for tool in missing {
        println!();
        println!("To install {}:", tool.name);
        for distro in Distro::ALL {
            let command = tool.install_command(distro).join(" ");
            println!("  {:<11} {command}", distro.name());
        }
    }
    println!();
    println!("Run `cargo xtask setup --install` to run these commands after confirming each.");
}

fn install(missing: &[&Tool]) -> Result<()> {
    let Some(distro) = Distro::detect() else {
        print_install_commands(missing);
        bail!("could not detect a supported Linux distribution for --install");
    };
    for tool in missing {
        let command = tool.install_command(distro);
        if !confirm(&format!("Run `{}`?", command.join(" ")))? {
            continue;
        }
        let status = Command::new(&command[0])
            .args(&command[1..])
            .status()
            .with_context(|| format!("failed to start `{}`", command[0]))?;
        if !status.success() {
            bail!("`{}` failed with {status}", command.join(" "));
        }
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}
