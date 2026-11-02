//! `cargo xtask fuzz`: runs one host fuzz target, and the replay of every
//! stored corpus that `cargo xtask test` does.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The cargo-fuzz projects, relative to the repository root.
const FUZZ_DIRS: [&str; 2] = ["kernel/fuzz", "boot/mikrosboot/fuzz"];

#[derive(clap::Args)]
pub struct Args {
    /// The target to run.
    pub target: String,
    /// How long to fuzz, in seconds.
    #[arg(long, default_value_t = 60)]
    pub time: u64,
}

/// The targets of the project in `dir`: the files in `fuzz_targets`.
fn targets(dir: &Path) -> Result<Vec<String>> {
    let sources = dir.join("fuzz_targets");
    let mut names: Vec<String> = std::fs::read_dir(&sources)
        .with_context(|| format!("reading {}", sources.display()))?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".rs").map(str::to_owned))
        .collect();
    names.sort();
    Ok(names)
}

/// Every target with its project directory.
fn all_targets() -> Result<Vec<(PathBuf, String)>> {
    let root = crate::workspace_root();
    let mut all = Vec::new();
    for dir in FUZZ_DIRS {
        let dir = root.join(dir);
        all.extend(targets(&dir)?.into_iter().map(|t| (dir.clone(), t)));
    }
    Ok(all)
}

/// Runs `cargo fuzz run` for `target` in `dir` with debug assertions, an
/// optimized build and AddressSanitizer, passing `extra`.
fn cargo_fuzz(dir: &Path, target: &str, extra: &[String]) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .current_dir(crate::workspace_root())
        .args(["fuzz", "run", "--fuzz-dir"])
        .arg(dir)
        .args(["-O", "-a", "--sanitizer", "address", target])
        .args(extra)
        .status()
        .context("failed to run cargo fuzz; install it with `cargo xtask setup`")?;
    if !status.success() {
        bail!("fuzz target {target} failed");
    }
    Ok(())
}

pub fn run(args: &Args) -> Result<()> {
    let all = all_targets()?;
    let Some((dir, target)) = all.iter().find(|(_, t)| *t == args.target) else {
        let names: Vec<&str> = all.iter().map(|(_, t)| t.as_str()).collect();
        bail!(
            "no fuzz target {}; the targets are {}",
            args.target,
            names.join(", ")
        );
    };
    cargo_fuzz(
        dir,
        target,
        &["--".into(), format!("-max_total_time={}", args.time)],
    )
}

/// Runs every target once over each input of its stored corpus.
pub fn replay_corpora() -> Result<()> {
    for (dir, target) in all_targets()? {
        let corpus = dir.join("corpus").join(&target);
        println!("replaying {}", corpus.display());
        cargo_fuzz(
            &dir,
            &target,
            &[corpus.display().to_string(), "--".into(), "-runs=0".into()],
        )?;
    }
    Ok(())
}
