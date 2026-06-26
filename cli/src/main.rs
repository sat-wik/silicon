use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use checker::{Noted, Severity};
use clap::Parser;

/// Vendored RP2040 SVD, embedded at build time so the binary works
/// out-of-the-box with no config — see CLAUDE.md's "single static binary"
/// stack decision. `--svd` overrides it.
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
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Text,
    Sarif,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum FailOn {
    Error,
    Warning,
    None,
}

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
    let svd_xml = match &args.svd {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading SVD file {}", path.display()))?
        }
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
    for file in &files {
        let source = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        accesses.extend(fw_parse::extract_accesses(&source, file));
    }

    let result = checker::check(&model, &accesses);

    let report_text = match args.format {
        Format::Text => {
            let mut s = report::render_text(&result.findings);
            if args.notes {
                s.push('\n');
                s.push_str(&render_notes(&result.notes));
            }
            s
        }
        Format::Sarif => serde_json::to_string_pretty(&report::render_sarif(&result.findings))
            .context("serializing SARIF")?,
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

fn render_notes(notes: &[Noted]) -> String {
    let mut s = String::from("notes:\n");
    for n in notes {
        s.push_str(&format!("  {}:{}: {}\n", n.file.display(), n.line, n.note));
    }
    s
}
