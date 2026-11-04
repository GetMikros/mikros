//! The `midlc` command: checks MIDL files and writes what they describe.
//!
//! Usage: `midlc [--version N] [--json FILE] [--rust FILE --kernel PATH] FILE.midl...`

use std::process::ExitCode;

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("midlc: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let (mut version, mut json, mut inputs) = (None, None, Vec::new());
    let (mut rust, mut kernel) = (None, None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--version" => {
                let v = value("--version")?;
                version = Some(v.parse().map_err(|_| format!("invalid version `{v}`"))?);
            }
            "--json" => json = Some(value("--json")?),
            "--rust" => rust = Some(value("--rust")?),
            "--kernel" => kernel = Some(value("--kernel")?),
            _ if arg.starts_with("--") => return Err(format!("unknown option `{arg}`")),
            _ => inputs.push(arg),
        }
    }
    if rust.is_some() != kernel.is_some() {
        return Err("--rust and --kernel go together".into());
    }
    if inputs.is_empty() {
        return Err(
            "usage: midlc [--version N] [--json FILE] [--rust FILE --kernel PATH] FILE.midl..."
                .into(),
        );
    }
    let texts = inputs
        .iter()
        .map(|p| std::fs::read_to_string(p).map_err(|e| format!("reading {p}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let sources: Vec<(&str, &str)> = inputs
        .iter()
        .map(String::as_str)
        .zip(texts.iter().map(String::as_str))
        .collect();
    let model = midlc::compile(&sources, version).map_err(|e| e.to_string())?;
    if let Some(path) = json {
        let text = serde_json::to_string_pretty(&midlc::json::describe(&model))
            .expect("JSON values print");
        std::fs::write(&path, text + "\n").map_err(|e| format!("writing {path}: {e}"))?;
    }
    if let (Some(path), Some(kernel)) = (rust, kernel) {
        let text = midlc::rust::generate(&model, &midlc::rust::Options { kernel });
        std::fs::write(&path, text).map_err(|e| format!("writing {path}: {e}"))?;
    }
    Ok(())
}
