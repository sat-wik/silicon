use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use checker::Severity;
use clap::Parser;

const VENDORED_RP2040_SVD: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/rp2040.svd"));

#[derive(Parser)]
#[command(name = "silicon", version, about = "RP2040 firmware register-correctness verifier")]
struct Args {
    /// Firmware C/C++ files or directories to check (directories are scanned for *.c files).
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// CMSIS-SVD file to check against. Defaults to the vendored RP2040 SVD.
    #[arg(long)]
    svd: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,

    /// Minimum severity that causes a non-zero exit code. `none` always exits 0.
    #[arg(long, value_enum, default_value_t = FailOn::Error)]
    fail_on: FailOn,

    /// Write the report to this file instead of stdout.
    #[arg(long, short)]
    output: Option<PathBuf>,

    /// Also print informational notes (unresolved accesses, unverifiable fields, etc).
    #[arg(long)]
    notes: bool,

    /// Disable colour output (auto-disabled when stdout is not a terminal).
    #[arg(long)]
    no_color: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format { Text, Sarif }

#[derive(Clone, Copy, clap::ValueEnum)]
enum FailOn { Error, Warning, None }

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("silicon: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> anyhow::Result<ExitCode> {
    // Color: on by default when writing to a terminal, off otherwise.
    let color = !args.no_color && args.output.is_none() && std::io::stdout().is_terminal();

    let svd_xml = match &args.svd {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading SVD file {}", path.display()))?,
        None => VENDORED_RP2040_SVD.to_string(),
    };
    let model = svd_model::Model::from_svd_str(&svd_xml).context("parsing SVD")?;

    let mut files = Vec::new();
    for p in &args.paths {
        collect_c_files(p, &mut files)
            .with_context(|| format!("scanning {}", p.display()))?;
    }
    if files.is_empty() {
        anyhow::bail!("no .c files found in the given paths");
    }

    let mut accesses = Vec::new();
    let mut hal_calls = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file)
            .with_context(|| format!("reading {}", file.display()))?;
        accesses.extend(fw_parse::extract_accesses(&source, file));
        hal_calls.extend(fw_parse::extract_hal_calls(&source, file));
    }

    let reg_result = checker::check(&model, &accesses);
    let hal_result = checker::check_hal_calls(&model, &hal_calls);
    let mut result = reg_result;
    result.findings.extend(hal_result.findings);
    result.notes.extend(hal_result.notes);
    result.findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    result.notes.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    let report_text = match args.format {
        Format::Text => {
            let mut s = report::render_text(&result.findings, &result.notes, color);
            if args.notes {
                s.push_str(&report::render_notes_text(&result.notes, color));
            }
            s
        }
        Format::Sarif => serde_json::to_string_pretty(
            &report::render_sarif(&result.findings, &result.notes)
        ).context("serializing SARIF")?,
    };

    match &args.output {
        Some(path) => std::fs::write(path, &report_text)
            .with_context(|| format!("writing report to {}", path.display()))?,
        None => print!("{report_text}"),
    }

    let threshold = match args.fail_on {
        FailOn::Error => Some(Severity::Error),
        FailOn::Warning => Some(Severity::Warning),
        FailOn::None => None,
    };
    let should_fail = threshold.is_some_and(|t| result.findings.iter().any(|f| f.severity >= t));

    Ok(if should_fail { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn collect_c_files(path: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            collect_c_files(&entry.path(), out)?;
        }
    } else if path.extension().is_some_and(|e| e == "c") {
        out.push(path.to_path_buf());
    }
    Ok(())
}
