use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context;
use checker::Severity;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

const VENDORED_RP2040_SVD: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/rp2040.svd"));
const CONFIG_FILE: &str = ".silicon.toml";

#[derive(Parser)]
#[command(name = "silicon", version, about = "RP2040 firmware register-correctness verifier")]
struct Args {
    /// Firmware C/C++ files or directories to check.
    /// When paths are given without a subcommand, silicon runs a check.
    #[arg()]
    paths: Vec<PathBuf>,

    /// CMSIS-SVD file to check against. Defaults to the vendored RP2040 SVD.
    #[arg(long, global = true)]
    svd: Option<PathBuf>,

    /// Output format for the check command.
    #[arg(long, value_enum)]
    format: Option<Format>,

    /// Minimum severity that causes a non-zero exit code (`none` always exits 0).
    #[arg(long, value_enum)]
    fail_on: Option<FailOn>,

    /// Write the report to this file instead of stdout.
    #[arg(long, short)]
    output: Option<PathBuf>,

    /// Also print informational notes (unresolved accesses, unverifiable fields, etc).
    #[arg(long)]
    notes: bool,

    /// Also scan *.h header files.
    #[arg(long)]
    scan_headers: bool,

    /// Disable colour output (auto-disabled when stdout is not a terminal).
    #[arg(long)]
    no_color: bool,

    /// Re-run the check whenever a source file changes. Exits only on Ctrl-C.
    #[arg(long)]
    watch: bool,

    /// Suppress findings that match a previously-written baseline file.
    #[arg(long, value_name = "FILE")]
    baseline: Option<PathBuf>,

    /// Write all current findings to a baseline file (then exit 0).
    /// Future runs with --baseline suppress these findings and only report new ones.
    #[arg(long, value_name = "FILE")]
    write_baseline: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<SubCmd>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Format { Text, Sarif }

#[derive(Debug, Clone, Copy, clap::ValueEnum, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum FailOn { Error, Warning, None }

#[derive(Subcommand)]
enum SubCmd {
    /// Explain what a rule checks, grounded in the SVD, with a C example.
    Explain {
        /// Rule ID to explain (e.g. field-value-not-in-enum).
        rule_id: String,
    },
    /// List all peripherals in the SVD with their base addresses.
    ListPeripherals,
    /// List all registers in a peripheral with offsets and field counts.
    ListRegisters {
        /// Peripheral name (e.g. CLOCKS, SIO, IO_BANK0).
        peripheral: String,
    },
    /// List all fields in a register with bit ranges and allowed values.
    ListFields {
        /// Peripheral name.
        peripheral: String,
        /// Register name.
        register: String,
    },
    /// Install a git pre-commit hook that runs silicon before each commit.
    InstallHook {
        /// Path to the git repository root (defaults to current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

/// All fields optional — CLI flags always override config-file values.
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
    if !path.exists() { return Ok(Config::default()); }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {CONFIG_FILE}"))?;
    toml::from_str(&text).with_context(|| format!("parsing {CONFIG_FILE}"))
}

// ── Baseline ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct BaselineFile {
    version: u32,
    findings: Vec<BaselineEntry>,
}

#[derive(Serialize, Deserialize)]
struct BaselineEntry {
    file: String,
    line: usize,
    #[serde(rename = "rule")]
    rule_id: String,
}

fn load_baseline(path: &Path) -> anyhow::Result<Vec<BaselineEntry>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading baseline {}", path.display()))?;
    let bf: BaselineFile = serde_json::from_str(&text)
        .with_context(|| format!("parsing baseline {}", path.display()))?;
    Ok(bf.findings)
}

fn write_baseline(path: &Path, findings: &[checker::Finding]) -> anyhow::Result<()> {
    let entries: Vec<BaselineEntry> = findings.iter().map(|f| BaselineEntry {
        file: f.file.to_string_lossy().into_owned(),
        line: f.line,
        rule_id: f.kind.rule_id().to_string(),
    }).collect();
    let bf = BaselineFile { version: 1, findings: entries };
    let json = serde_json::to_string_pretty(&bf).context("serializing baseline")?;
    std::fs::write(path, json)
        .with_context(|| format!("writing baseline to {}", path.display()))
}

fn suppress_baselined(
    findings: Vec<checker::Finding>,
    baseline: &[BaselineEntry],
) -> Vec<checker::Finding> {
    findings.into_iter().filter(|f| {
        let file_str = f.file.to_string_lossy();
        !baseline.iter().any(|b| {
            b.line == f.line
                && b.rule_id == f.kind.rule_id()
                && (b.file == file_str || b.file == f.file.to_str().unwrap_or(""))
        })
    }).collect()
}

// ── RunOpts ──────────────────────────────────────────────────────────────────

struct RunOpts<'a> {
    model: &'a svd_model::Model,
    notes: bool,
    format: Format,
    fail_on: FailOn,
    output: Option<&'a Path>,
    color: bool,
    baseline: Option<Vec<BaselineEntry>>,
    write_baseline: Option<&'a Path>,
}

// ── main / run ───────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => code,
        Err(e) => { eprintln!("silicon: {e:#}"); ExitCode::from(2) }
    }
}

fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let color = !args.no_color && args.output.is_none() && std::io::stdout().is_terminal();

    // Subcommands that don't need the SVD model.
    match &args.command {
        Some(SubCmd::InstallHook { repo }) => return run_install_hook(repo.as_deref()),
        Some(SubCmd::Explain { rule_id }) => return run_explain(rule_id, color),
        _ => {}
    }

    let cfg = load_config()?;
    let svd_path = args.svd.clone().or(cfg.svd.clone());
    let svd_xml = match &svd_path {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading SVD {}", path.display()))?,
        None => VENDORED_RP2040_SVD.to_string(),
    };
    let model = svd_model::Model::from_svd_str(&svd_xml).context("parsing SVD")?;

    // Subcommands that need the SVD model but not firmware paths.
    match &args.command {
        Some(SubCmd::ListPeripherals) => return run_list_peripherals(&model, color),
        Some(SubCmd::ListRegisters { peripheral }) =>
            return run_list_registers(&model, peripheral, color),
        Some(SubCmd::ListFields { peripheral, register }) =>
            return run_list_fields(&model, peripheral, register, color),
        _ => {}
    }

    // Default: check command.
    let paths: Vec<PathBuf> = if args.paths.is_empty() {
        cfg.paths.clone().unwrap_or_default()
    } else {
        args.paths.clone()
    };
    if paths.is_empty() {
        anyhow::bail!(
            "no paths given — pass paths on the command line or set `paths` in .silicon.toml"
        );
    }

    let format = args.format.or(cfg.format).unwrap_or(Format::Text);
    let fail_on = args.fail_on.or(cfg.fail_on).unwrap_or(FailOn::Error);
    let output = args.output.clone().or(cfg.output.clone());
    let notes = args.notes || cfg.notes.unwrap_or(false);
    let scan_headers = args.scan_headers || cfg.scan_headers.unwrap_or(false);

    let baseline = args.baseline.as_deref().map(load_baseline).transpose()?;
    let write_bl = args.write_baseline.as_deref();

    let opts = RunOpts {
        model: &model,
        notes,
        format,
        fail_on,
        output: output.as_deref(),
        color,
        baseline,
        write_baseline: write_bl,
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

// ── check ─────────────────────────────────────────────────────────────────────

fn run_once(files: &[PathBuf], opts: &RunOpts<'_>) -> anyhow::Result<ExitCode> {
    let sources: Vec<(String, PathBuf)> = files.iter().map(|f| {
        std::fs::read_to_string(f)
            .with_context(|| format!("reading {}", f.display()))
            .map(|s| (s, f.clone()))
    }).collect::<anyhow::Result<_>>()?;

    let macro_inputs: Vec<(&str, &Path)> = sources.iter()
        .map(|(src, path)| (src.as_str(), path.as_path())).collect();
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

    // Write baseline if requested (before suppression).
    if let Some(bl_path) = opts.write_baseline {
        write_baseline(bl_path, &result.findings)?;
        let n = result.findings.len();
        eprintln!("silicon: wrote {n} finding{} to baseline {}", if n == 1 { "" } else { "s" }, bl_path.display());
        return Ok(ExitCode::SUCCESS);
    }

    // Suppress baselined findings.
    if let Some(baseline) = &opts.baseline {
        let before = result.findings.len();
        result.findings = suppress_baselined(result.findings, baseline);
        let suppressed = before - result.findings.len();
        if suppressed > 0 {
            eprintln!("silicon: suppressed {suppressed} baseline finding{}", if suppressed == 1 { "" } else { "s" });
        }
    }

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
        FailOn::Error   => Some(Severity::Error),
        FailOn::Warning => Some(Severity::Warning),
        FailOn::None    => None,
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
        debouncer.watcher().watch(p, RecursiveMode::Recursive)
            .with_context(|| format!("watching {}", p.display()))?;
    }
    eprintln!("silicon: watching {} path(s) — press Ctrl-C to stop", paths.len());

    let run_check = |scan_h: bool| -> anyhow::Result<()> {
        let mut files = Vec::new();
        for p in paths {
            collect_c_files(p, scan_h, &mut files)
                .with_context(|| format!("scanning {}", p.display()))?;
        }
        if !files.is_empty() { let _ = run_once(&files, opts); }
        Ok(())
    };

    run_check(scan_headers)?;
    loop {
        match rx.recv() {
            Ok(Ok(_)) => {
                if opts.color { eprint!("\x1b[2m"); }
                eprintln!("─── re-checking ───────────────────────────────────────────────");
                if opts.color { eprint!("\x1b[0m"); }
                run_check(scan_headers)?;
            }
            Ok(Err(e)) => eprintln!("silicon: watch error: {e}"),
            Err(_) => break,
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn collect_c_files(path: &Path, scan_headers: bool, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for entry in entries { collect_c_files(&entry.path(), scan_headers, out)?; }
    } else if let Some(ext) = path.extension() {
        if ext == "c" || (scan_headers && ext == "h") {
            out.push(path.to_path_buf());
        }
    }
    Ok(())
}

// ── explain ──────────────────────────────────────────────────────────────────

struct ExplainEntry {
    rule_id:     &'static str,
    what:        &'static str,
    svd_fact:    &'static str,
    bad_example: &'static str,
    ok_example:  &'static str,
}

const EXPLANATIONS: &[ExplainEntry] = &[
    ExplainEntry {
        rule_id: "field-value-not-in-enum",
        what: "A value written to a bitfield is not one of the allowed values \
               listed in the SVD's <enumeratedValues> for that field.",
        svd_fact: "Only checked when the SVD defines <enumeratedValues> for the field. \
                   If the SVD has no enum, the value is unverifiable and silicon emits \
                   a note instead (invariant 6).",
        bad_example: "// AUXSRC = 12 — not in SVD enum {0..10}\nclocks_hw->clk_gpout0_ctrl = 12u << 5;",
        ok_example:  "// AUXSRC = 0 (clksrc_pll_sys) — valid per SVD\nclocks_hw->clk_gpout0_ctrl = 0u << 5;",
    },
    ExplainEntry {
        rule_id: "nonexistent-register",
        what: "The register name doesn't appear in the SVD for the given peripheral. \
               Usually a mistyped name or wrong peripheral struct.",
        svd_fact: "Checked against the exhaustive register list the SVD defines for \
                   each peripheral.",
        bad_example: "sio_hw->not_a_real_register = 1;  // no such register in SIO",
        ok_example:  "sio_hw->gpio_out = 1u << 25;       // GPIO_OUT exists in SVD",
    },
    ExplainEntry {
        rule_id: "address-not-a-register",
        what: "A literal address falls inside a peripheral's address block but doesn't \
               align with any defined register's start address.",
        svd_fact: "Resolved via the SVD address map: peripheral base + register offset. \
                   Off-by-one errors in offset arithmetic commonly trigger this.",
        bad_example: "// SIO base is 0xd0000000; offset 0x1 is not a register boundary\n\
                      *(volatile uint32_t *)(0xd0000001u) = 1;",
        ok_example:  "// GPIO_OUT is at SIO base + 0x010\n\
                      *(volatile uint32_t *)(0xd0000010u) = 1u << 25;",
    },
    ExplainEntry {
        rule_id: "value-exceeds-register-width",
        what: "The value written is too large to fit in the register's declared bit width.",
        svd_fact: "Checked against the SVD <size> element for the register \
                   (typically 32 bits, but some peripherals define narrower registers).",
        bad_example: "// FBDIV_INT is 12 bits wide; 5000 > 4095\npll_sys_hw->fbdiv_int = 5000;",
        ok_example:  "// 125 fits in 12 bits\npll_sys_hw->fbdiv_int = 125;",
    },
    ExplainEntry {
        rule_id: "value-sets-undefined-bits",
        what: "The value has 1-bits outside all defined fields — reserved or \
               undocumented bits that the SVD doesn't assign to any field.",
        svd_fact: "The defined-bit mask is OR'd from all field masks for the register. \
                   value & ~defined_mask != 0 triggers this warning.",
        bad_example: "// Bit 31 is not assigned to any field in PLL_SYS.FBDIV_INT\n\
                      pll_sys_hw->fbdiv_int = 0x80000064u;",
        ok_example:  "// Only the FBDIV_INT field bits (0..11) are set\n\
                      pll_sys_hw->fbdiv_int = 100;",
    },
    ExplainEntry {
        rule_id: "write-to-read-only-register",
        what: "The SVD marks the entire register as read-only, but firmware writes to it.",
        svd_fact: "Checked against the SVD <access>read-only</access> annotation on \
                   the register element.",
        bad_example: "sio_hw->cpuid = 5;  // CPUID is read-only in the SVD",
        ok_example:  "uint32_t id = sio_hw->cpuid;  // read is fine",
    },
    ExplainEntry {
        rule_id: "write-to-read-only-field",
        what: "A specific field is read-only in the SVD, and the write sets bits \
               within that field.",
        svd_fact: "Checked against <access>read-only</access> on the field element. \
                   The field's bit mask is used to detect which bits the write touches.",
        bad_example: "// CPUID field is read-only\nsio_hw->cpuid = 5;",
        ok_example:  "// Write only to writable fields",
    },
    ExplainEntry {
        rule_id: "hal-call-pin-out-of-range",
        what: "A gpio_* HAL call passes a pin number outside the RP2040's GPIO range \
               (GPIO0–GPIO29).",
        svd_fact: "Grounded in the SVD: GPIO30 and above have no IO_BANK0 register \
                   entry. The RP2040 has exactly 30 GPIOs.",
        bad_example: "gpio_init(30);   // GPIO30 doesn't exist on RP2040",
        ok_example:  "gpio_init(25);   // GPIO25 = onboard LED on Pico",
    },
    ExplainEntry {
        rule_id: "hal-call-funcsel-not-in-enum",
        what: "gpio_set_function is called with a function selector value not in the \
               SVD's FUNCSEL enum for that pin.",
        svd_fact: "Checked against IO_BANK0.GPIO{pin}_CTRL.FUNCSEL's \
                   <enumeratedValues> in the SVD.",
        bad_example: "gpio_set_function(25, 99);  // 99 is not a valid FUNCSEL",
        ok_example:  "gpio_set_function(25, 5);   // 5 = SIO function, valid per SVD",
    },
    ExplainEntry {
        rule_id: "hal-call-index-out-of-range",
        what: "A PWM slice, DMA channel, or ADC channel index doesn't correspond to \
               a peripheral instance defined in the SVD.",
        svd_fact: "PWM: slice validated via CH{n}_TOP register existence (slices 0–7). \
                   DMA: channel via CH{n}_CTRL_TRIG (channels 0–11). \
                   ADC: channel vs AINSEL field bit width (3 bits → max 7).",
        bad_example: "pwm_set_wrap(8, 1000);          // only slices 0-7 exist\n\
                      dma_channel_configure(12, ...); // only channels 0-11 exist",
        ok_example:  "pwm_set_wrap(7, 1000);          // slice 7 — last valid slice\n\
                      dma_channel_configure(0, ...);  // channel 0 is valid",
    },
    ExplainEntry {
        rule_id: "hal-call-instance-not-in-svd",
        what: "A UART, SPI, or I2C HAL call's instance argument doesn't match any \
               peripheral in the SVD. The RP2040 has uart0/uart1, spi0/spi1, i2c0/i2c1.",
        svd_fact: "The instance name is mapped to a SVD peripheral (uart0 → UART0) \
                   and checked with model.peripheral(). If absent, no such instance exists.",
        bad_example: "uart_init(uart2, 115200);  // RP2040 only has uart0 and uart1",
        ok_example:  "uart_init(uart1, 115200);  // uart1 exists in SVD",
    },
];

fn run_explain(rule_id: &str, color: bool) -> anyhow::Result<ExitCode> {
    let b  = if color { "\x1b[1m" }    else { "" };
    let cy = if color { "\x1b[36m" }   else { "" };
    let gr = if color { "\x1b[32m" }   else { "" };
    let rd = if color { "\x1b[31m" }   else { "" };
    let rs = if color { "\x1b[0m" }    else { "" };

    let Some(entry) = EXPLANATIONS.iter().find(|e| e.rule_id == rule_id) else {
        let ids: Vec<&str> = EXPLANATIONS.iter().map(|e| e.rule_id).collect();
        eprintln!("silicon: unknown rule '{rule_id}'");
        eprintln!("known rules: {}", ids.join(", "));
        return Ok(ExitCode::from(2));
    };

    println!("\n {b}ⓘ  {}{rs}\n", entry.rule_id);

    println!(" {b}What it checks{rs}");
    for line in wrap_text(entry.what, 70) {
        println!("   {line}");
    }
    println!();

    println!(" {b}SVD fact{rs}");
    for line in wrap_text(entry.svd_fact, 70) {
        println!("   {cy}{line}{rs}");
    }
    println!();

    println!(" {b}Example — triggers this rule{rs}");
    for line in entry.bad_example.lines() {
        println!("   {rd}{line}{rs}");
    }
    println!();

    println!(" {b}Example — passes{rs}");
    for line in entry.ok_example.lines() {
        println!("   {gr}{line}{rs}");
    }
    println!();

    Ok(ExitCode::SUCCESS)
}

/// Word-wraps `text` to `max_width` columns, returning one line per element.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= max_width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current = word.to_string();
        }
    }
    if !current.is_empty() { lines.push(current); }
    lines
}

// ── list-peripherals ─────────────────────────────────────────────────────────

fn run_list_peripherals(model: &svd_model::Model, color: bool) -> anyhow::Result<ExitCode> {
    let b  = if color { "\x1b[1m" } else { "" };
    let d  = if color { "\x1b[2m" } else { "" };
    let rs = if color { "\x1b[0m" } else { "" };
    let cy = if color { "\x1b[36m" } else { "" };

    let mut peripherals: Vec<_> = model.peripherals().iter().collect();
    peripherals.sort_by_key(|p| &p.name);

    println!("\n {b}{:<22}  {:<14}  REGISTERS{rs}", "PERIPHERAL", "BASE");
    println!(" {d}{}{rs}", "─".repeat(46));

    for p in &peripherals {
        println!(" {cy}{:<22}{rs}  0x{:08x}      {:>5}",
            p.name, p.base_address, p.registers.len());
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

// ── list-registers ───────────────────────────────────────────────────────────

fn run_list_registers(
    model: &svd_model::Model,
    peripheral: &str,
    color: bool,
) -> anyhow::Result<ExitCode> {
    let b  = if color { "\x1b[1m" }  else { "" };
    let d  = if color { "\x1b[2m" }  else { "" };
    let rs = if color { "\x1b[0m" }  else { "" };
    let cy = if color { "\x1b[36m" } else { "" };

    let p_upper = peripheral.to_uppercase();
    let Some(p) = model.peripheral(&p_upper) else {
        let mut all: Vec<_> = model.peripherals().iter().map(|p| p.name.as_str()).collect();
        all.sort_unstable();
        eprintln!("silicon: no peripheral '{p_upper}' in SVD");
        eprintln!("available: {}", all.join(", "));
        return Ok(ExitCode::from(2));
    };

    println!("\n {b}{} registers{rs}  {d}(base 0x{:08x}){rs}\n", p.name, p.base_address);
    println!(" {b}{:<34}  {:>8}   {:>4}   {:<10}  FIELDS{rs}", "REGISTER", "OFFSET", "BITS", "ACCESS");
    println!(" {d}{}{rs}", "─".repeat(72));

    let mut regs = p.registers.clone();
    regs.sort_by_key(|r| r.address_offset);
    for r in &regs {
        let access = format!("{:?}", r.access).to_lowercase();
        println!(" {cy}{:<34}{rs}  0x{:06x}   {:>4}   {:<10}  {}",
            r.name, r.address_offset, r.size_bits, access, r.fields.len());
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

// ── list-fields ──────────────────────────────────────────────────────────────

fn run_list_fields(
    model: &svd_model::Model,
    peripheral: &str,
    register: &str,
    color: bool,
) -> anyhow::Result<ExitCode> {
    let b  = if color { "\x1b[1m" }  else { "" };
    let d  = if color { "\x1b[2m" }  else { "" };
    let rs = if color { "\x1b[0m" }  else { "" };
    let cy = if color { "\x1b[36m" } else { "" };
    let di = if color { "\x1b[2m" }  else { "" };

    let p_upper = peripheral.to_uppercase();
    let r_upper = register.to_uppercase();

    let Some(p) = model.peripheral(&p_upper) else {
        eprintln!("silicon: no peripheral '{p_upper}' in SVD");
        return Ok(ExitCode::from(2));
    };
    let Some(reg) = p.registers.iter().find(|r| r.name == r_upper) else {
        eprintln!("silicon: no register '{r_upper}' in peripheral '{p_upper}'");
        return Ok(ExitCode::from(2));
    };

    let access = format!("{:?}", reg.access).to_lowercase();
    println!("\n {b}{}.{}{rs}  {d}(offset 0x{:03x}, {}-bit, {}){rs}\n",
        p.name, reg.name, reg.address_offset, reg.size_bits, access);

    if reg.fields.is_empty() {
        println!("  {d}(no fields defined in SVD){rs}\n");
        return Ok(ExitCode::SUCCESS);
    }

    println!(" {b}{:<20}  {:>12}   {:<10}  ALLOWED VALUES{rs}", "FIELD", "BITS", "ACCESS");
    println!(" {d}{}{rs}", "─".repeat(72));

    let mut fields = reg.fields.clone();
    fields.sort_by_key(|f| f.bit_offset);
    for f in &fields {
        let hi = f.bit_offset + f.bit_width - 1;
        let bits = if f.bit_width == 1 {
            format!("[{}]", f.bit_offset)
        } else {
            format!("[{}:{}]", hi, f.bit_offset)
        };
        let access = format!("{:?}", f.access).to_lowercase();
        let values = match &f.allowed_values {
            Some(vals) => {
                let s = vals.iter()
                    .map(|v| format!("{}={}", v.name, v.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                // Truncate very long enum lists for the terminal.
                if s.len() > 50 {
                    format!("{} … ({} values)", &s[..47], vals.len())
                } else {
                    s
                }
            }
            None => format!("{di}(no SVD enum){rs}"),
        };
        println!(" {cy}{:<20}{rs}  {:>12}   {:<10}  {}",
            f.name, bits, access, values);
    }
    println!();
    Ok(ExitCode::SUCCESS)
}

// ── install-hook ─────────────────────────────────────────────────────────────

fn run_install_hook(repo: Option<&Path>) -> anyhow::Result<ExitCode> {
    let repo = repo.unwrap_or(Path::new("."));
    let hooks_dir = repo.join(".git").join("hooks");
    if !hooks_dir.exists() {
        anyhow::bail!(
            "{} is not a git repository (no .git/hooks directory)",
            repo.display()
        );
    }

    let hook_path = hooks_dir.join("pre-commit");
    let hook_content = concat!(
        "#!/bin/sh\n",
        "# silicon: RP2040 register-correctness pre-commit check\n",
        "# Generated by 'silicon install-hook'. Remove this file to disable.\n",
        "if ! silicon check 2>/dev/null; then\n",
        "    silicon check\n",
        "    echo ''\n",
        "    echo 'silicon: pre-commit check failed — fix register violations before committing'\n",
        "    exit 1\n",
        "fi\n",
    );

    if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)?;
        if existing.contains("silicon") {
            eprintln!("silicon: pre-commit hook already installed at {}", hook_path.display());
            return Ok(ExitCode::SUCCESS);
        }
        // Append to existing hook rather than overwrite it.
        let appended = format!("{}\n{}", existing.trim_end(), hook_content);
        std::fs::write(&hook_path, appended)
            .with_context(|| format!("writing {}", hook_path.display()))?;
        eprintln!("silicon: appended silicon check to existing hook at {}", hook_path.display());
    } else {
        std::fs::write(&hook_path, hook_content)
            .with_context(|| format!("writing {}", hook_path.display()))?;
        eprintln!("silicon: installed pre-commit hook at {}", hook_path.display());
    }

    // Make the hook executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    eprintln!("silicon: the hook runs 'silicon check' before each commit.");
    eprintln!("         Add .silicon.toml to set paths, or it will check the current directory.");
    Ok(ExitCode::SUCCESS)
}
