use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use checker::Severity;
use clap::Parser;
use serde::Deserialize;

const VENDORED_RP2040_SVD: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/rp2040.svd"));
const CONFIG_FILE: &str = ".silicon.toml";

#[derive(Parser)]
#[command(name = "silicon", version, about = "RP2040 firmware register-correctness verifier")]
struct Args {
    /// Firmware C/C++ files or directories to check (directories are scanned for *.c files).
    /// If omitted, falls back to `paths` in .silicon.toml.
    #[arg()]
    paths: Vec<PathBuf>,

    /// CMSIS-SVD file to check against. Defaults to the vendored RP2040 SVD.
    #[arg(long)]
    svd: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum)]
    format: Option<Format>,

    /// Minimum severity that causes a non-zero exit code. `none` always exits 0.
    #[arg(long, value_enum)]
    fail_on: Option<FailOn>,

    /// Write the report to this file instead of stdout.
    #[arg(long, short)]
    output: Option<PathBuf>,

    /// Also print informational notes (unresolved accesses, unverifiable fields, etc).
    #[arg(long)]
    notes: bool,

    /// Also scan *.h header files for register accesses and macro definitions.
    #[arg(long)]
    scan_headers: bool,

    /// Disable colour output (auto-disabled when stdout is not a terminal).
    #[arg(long)]
    no_color: bool,

    /// Re-run the check whenever a source file changes. Exits only on Ctrl-C.
    #[arg(long)]
    watch: bool,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Format { Text, Sarif }

#[derive(Debug, Clone, Copy, clap::ValueEnum, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FailOn { Error, Warning, None }

/// Deserialized `.silicon.toml`. All fields are optional — missing = use the
/// CLI default. CLI flags always win over the config file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    paths: Option<Vec<PathBuf>>,
    svd: Option<PathBuf>,
    format: Option<Format>,
    #[serde(rename = "fail-on")]
    fail_on: Option<FailOn>,
    output: Option<PathBuf>,
    notes: Option<bool>,
    #[serde(rename = "scan-headers")]
    scan_headers: Option<bool>,
}

fn load_config() -> anyhow::Result<Config> {
    let path = Path::new(CONFIG_FILE);
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {CONFIG_FILE}"))?;
    toml::from_str(&text).with_context(|| format!("parsing {CONFIG_FILE}"))
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
    let cfg = load_config()?;

    // Merge: CLI arg wins over config file; config file wins over built-in default.
    let paths: Vec<PathBuf> = if args.paths.is_empty() {
        cfg.paths.clone().unwrap_or_default()
    } else {
        args.paths.clone()
    };
    if paths.is_empty() {
        anyhow::bail!("no paths given — pass paths on the command line or set `paths` in .silicon.toml");
    }

    let svd_path = args.svd.clone().or(cfg.svd.clone());
    let format = args.format.or(cfg.format).unwrap_or(Format::Text);
    let fail_on = args.fail_on.or(cfg.fail_on).unwrap_or(FailOn::Error);
    let output = args.output.clone().or(cfg.output.clone());
    let notes = args.notes || cfg.notes.unwrap_or(false);
    let scan_headers = args.scan_headers || cfg.scan_headers.unwrap_or(false);
    let color = !args.no_color && output.is_none() && std::io::stdout().is_terminal();

    let svd_xml = match &svd_path {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading SVD file {}", path.display()))?,
        None => VENDORED_RP2040_SVD.to_string(),
    };
    let model = svd_model::Model::from_svd_str(&svd_xml).context("parsing SVD")?;

    let opts = RunOpts {
        model: &model,
        notes,
        format,
        fail_on,
        output: output.as_deref(),
        color,
    };

    if args.watch {
        run_watch(&paths, scan_headers, &opts)
    } else {
        let mut files = Vec::new();
        for p in &paths {
            collect_c_files(p, scan_headers, &mut files)
                .with_context(|| format!("scanning {}", p.display()))?;
        }
        if files.is_empty() {
            anyhow::bail!("no .c/.h files found in the given paths");
        }
        run_once(&files, &opts)
    }
}

struct RunOpts<'a> {
    model: &'a svd_model::Model,
    notes: bool,
    format: Format,
    fail_on: FailOn,
    output: Option<&'a Path>,
    color: bool,
}

fn run_once(files: &[PathBuf], opts: &RunOpts<'_>) -> anyhow::Result<ExitCode> {
    let sources: Vec<(String, PathBuf)> = files
        .iter()
        .map(|f| {
            std::fs::read_to_string(f)
                .with_context(|| format!("reading {}", f.display()))
                .map(|s| (s, f.clone()))
        })
        .collect::<anyhow::Result<_>>()?;

    let macro_inputs: Vec<(&str, &Path)> = sources
        .iter()
        .map(|(src, path)| (src.as_str(), path.as_path()))
        .collect();
    let project_macros = fw_parse::build_project_macro_table(&macro_inputs);

    let mut accesses = Vec::new();
    let mut hal_calls = Vec::new();
    for (source, file) in &sources {
        accesses.extend(fw_parse::extract_accesses_with_macros(source, file, &project_macros));
        hal_calls.extend(fw_parse::extract_hal_calls_with_macros(source, file, &project_macros));
    }

    let reg_result = checker::check(opts.model, &accesses);
    let hal_result = checker::check_hal_calls(opts.model, &hal_calls);
    let mut result = reg_result;
    result.findings.extend(hal_result.findings);
    result.notes.extend(hal_result.notes);
    result.findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    result.notes.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    let report_text = match opts.format {
        Format::Text => {
            let mut s = report::render_text(&result.findings, &result.notes, opts.color);
            if opts.notes {
                s.push_str(&report::render_notes_text(&result.notes, opts.color));
            }
            s
        }
        Format::Sarif => serde_json::to_string_pretty(
            &report::render_sarif(&result.findings, &result.notes)
        ).context("serializing SARIF")?,
    };

    match opts.output {
        Some(path) => std::fs::write(path, &report_text)
            .with_context(|| format!("writing report to {}", path.display()))?,
        None => print!("{report_text}"),
    }

    let threshold = match opts.fail_on {
        FailOn::Error => Some(Severity::Error),
        FailOn::Warning => Some(Severity::Warning),
        FailOn::None => None,
    };
    let should_fail = threshold.is_some_and(|t| result.findings.iter().any(|f| f.severity >= t));
    Ok(if should_fail { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn run_watch(paths: &[PathBuf], scan_headers: bool, opts: &RunOpts<'_>) -> anyhow::Result<ExitCode> {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(300), tx)
        .context("creating file watcher")?;

    for p in paths {
        debouncer.watcher()
            .watch(p, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", p.display()))?;
    }

    eprintln!("silicon: watching {} path(s) — press Ctrl-C to stop", paths.len());

    // Run immediately, then re-run on each change.
    let run_check = |scan_h: bool| -> anyhow::Result<()> {
        let mut files = Vec::new();
        for p in paths {
            collect_c_files(p, scan_h, &mut files)
                .with_context(|| format!("scanning {}", p.display()))?;
        }
        if files.is_empty() { return Ok(()); }
        let _ = run_once(&files, opts);
        Ok(())
    };

    run_check(scan_headers)?;

    loop {
        match rx.recv() {
            Ok(Ok(_events)) => {
                // Print a visual separator so successive runs are distinct.
                if opts.color {
                    eprint!("\x1b[2m");
                }
                eprintln!("─── re-checking ───────────────────────────────────────────────");
                if opts.color {
                    eprint!("\x1b[0m");
                }
                run_check(scan_headers)?;
            }
            Ok(Err(errors)) => {
                eprintln!("silicon: watch error: {errors}");
            }
            Err(_) => break,
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn collect_c_files(path: &Path, scan_headers: bool, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            collect_c_files(&entry.path(), scan_headers, out)?;
        }
    } else if let Some(ext) = path.extension() {
        if ext == "c" || (scan_headers && ext == "h") {
            out.push(path.to_path_buf());
        }
    }
    Ok(())
}
