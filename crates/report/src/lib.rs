//! M4: renders checker findings as a terminal report or a SARIF document.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use checker::{Finding, Noted, Severity};

// ── ANSI colour palette ───────────────────────────────────────────────────────

struct C {
    reset:       &'static str,
    bold:        &'static str,
    dim:         &'static str,
    bold_white:  &'static str,
    bold_red:    &'static str,
    bold_yellow: &'static str,
    bold_cyan:   &'static str,
    cyan:        &'static str,
    // reverse-video badges (inverted foreground/background)
    rev_red:     &'static str,
    rev_yellow:  &'static str,
    rev_green:   &'static str,
}

impl C {
    fn new(color: bool) -> Self {
        if color {
            C {
                reset:       "\x1b[0m",
                bold:        "\x1b[1m",
                dim:         "\x1b[2m",
                bold_white:  "\x1b[1;37m",
                bold_red:    "\x1b[1;31m",
                bold_yellow: "\x1b[1;33m",
                bold_cyan:   "\x1b[1;36m",
                cyan:        "\x1b[36m",
                rev_red:     "\x1b[7;31m",
                rev_yellow:  "\x1b[7;33m",
                rev_green:   "\x1b[7;32m",
            }
        } else {
            C {
                reset:"", bold:"", dim:"", bold_white:"", bold_red:"",
                bold_yellow:"", bold_cyan:"", cyan:"", rev_red:"",
                rev_yellow:"", rev_green:"",
            }
        }
    }
    fn sev_fg(&self, s: Severity) -> &'static str {
        match s { Severity::Error => self.bold_red, Severity::Warning => self.bold_yellow }
    }
    fn sev_rev(&self, s: Severity) -> &'static str {
        match s { Severity::Error => self.rev_red, Severity::Warning => self.rev_yellow }
    }
}

// ── Source file cache ─────────────────────────────────────────────────────────

#[derive(Default)]
struct SourceCache(HashMap<PathBuf, Option<Vec<String>>>);

impl SourceCache {
    fn line(&mut self, file: &Path, line: usize) -> Option<&str> {
        let entry = self.0.entry(file.to_path_buf()).or_insert_with(|| {
            std::fs::read_to_string(file)
                .ok()
                .map(|s| s.lines().map(String::from).collect())
        });
        entry.as_ref()?.get(line.saturating_sub(1)).map(String::as_str)
    }
}

// ── Terminal report ───────────────────────────────────────────────────────────

pub fn render_text(findings: &[Finding], _notes: &[Noted], color: bool) -> String {
    let c = C::new(color);
    let mut src = SourceCache::default();
    let mut out = String::new();

    for (i, f) in findings.iter().enumerate() {
        if i > 0 {
            out.push_str(&separator(&c));
        }
        out.push_str(&render_finding(f, &mut src, &c));
    }

    out.push_str(&render_summary(findings, &c));
    out
}

fn render_finding(f: &Finding, src: &mut SourceCache, c: &C) -> String {
    let mut out = String::new();
    let (icon, sev_str) = match f.severity {
        Severity::Error   => ("✗", "error"),
        Severity::Warning => ("⚠", "warning"),
    };
    let sev_fg  = c.sev_fg(f.severity);
    let sev_rev = c.sev_rev(f.severity);
    let caret_ch = match f.severity { Severity::Error => '^', Severity::Warning => '~' };

    // ── Severity badge + rule id ──────────────────────────────────────────────
    out += &format!(
        " {rev} {icon} {sev_str} {rst}  {dim}{}{rst}\n",
        f.kind.rule_id(), rev = sev_rev, rst = c.reset, dim = c.dim,
    );

    // ── Headline ─────────────────────────────────────────────────────────────
    out += &format!(" {bw}{}{rst}\n", f.kind.title(), bw = c.bold_white, rst = c.reset);
    out.push('\n');

    // ── Source box ───────────────────────────────────────────────────────────
    let file = f.file.display().to_string();
    let line_w = digits(f.line).max(2);
    let bc = c.bold_cyan;
    let rst = c.reset;

    out += &format!("  {bc}╭─{rst} {bold}{file}{dim} · line {}{rst}\n",
        f.line, bold = c.bold, dim = c.dim, bc = bc, rst = rst);
    out += &format!("  {bc}│{rst}\n", bc = bc, rst = rst);

    let raw = src.line(&f.file, f.line)
        .map(str::to_string)
        .unwrap_or_else(|| format!("    {}", expr_text(f)));

    let expr = expr_text(f);

    // Source line with highlighted expression
    let colored_src = colorize_expr(&raw, &expr, sev_fg, c);
    out += &format!(
        "  {bc}│{rst}  {dim}{n:>w$}{rst}  {bc}│{rst}  {src}\n",
        n = f.line, w = line_w, src = colored_src,
        bc = bc, dim = c.dim, rst = rst,
    );

    // Caret underline
    let lead      = raw.len() - raw.trim_start().len();
    let col_off   = raw.find(expr.trim_start()).unwrap_or(lead);
    let caret_len = expr.len().min(raw.len().saturating_sub(col_off)).max(1);
    let carets    = caret_ch.to_string().repeat(caret_len);
    out += &format!(
        "  {bc}│{rst}  {sp:>w$}  {bc}│{rst}  {pad}{fg}{bold}{carets}{rst}\n",
        sp = "", w = line_w, pad = " ".repeat(col_off),
        fg = sev_fg, bold = c.bold,
        bc = bc, rst = rst,
    );

    // ── Citation / close ─────────────────────────────────────────────────────
    if let Some(cit) = f.kind.citation() {
        out += &format!("  {bc}│{rst}\n", bc = bc, rst = rst);
        let mut cit_lines = cit.lines();
        if let Some(first) = cit_lines.next() {
            out += &format!("  {bc}╰─{rst} {cyan}{}{rst}\n", first, cyan = c.cyan, bc = bc, rst = rst);
        }
        for line in cit_lines {
            out += &format!("       {cyan}{}{rst}\n", line, cyan = c.cyan, rst = rst);
        }
    } else {
        out += &format!("  {bc}╰──{rst}\n", bc = bc, rst = rst);
    }

    out
}

/// Wrap the part of `raw` that matches `expr` in color codes, leaving the
/// rest unchanged. Used to highlight the bad expression inside its source line.
fn colorize_expr(raw: &str, expr: &str, color: &'static str, c: &C) -> String {
    let needle = expr.trim_start();
    if let Some(pos) = raw.find(needle) {
        let end = (pos + needle.len()).min(raw.len());
        format!("{}{}{}{}{}", &raw[..pos], color, &raw[pos..end], c.reset, &raw[end..])
    } else {
        raw.to_string()
    }
}

fn separator(c: &C) -> String {
    format!("\n  {dim}{}{rst}\n\n", "─".repeat(60), dim = c.dim, rst = c.reset)
}

fn render_summary(findings: &[Finding], c: &C) -> String {
    let errors   = findings.iter().filter(|f| f.severity == Severity::Error).count();
    let warnings = findings.iter().filter(|f| f.severity == Severity::Warning).count();

    if errors == 0 && warnings == 0 {
        return format!("\n {rb} ✓ no findings {rst}\n\n",
            rb = c.rev_green, rst = c.reset);
    }

    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!("{rev} ✗ {n} error{s} {rst}",
            rev = c.rev_red, rst = c.reset, n = errors,
            s = if errors == 1 { "" } else { "s" }));
    }
    if warnings > 0 {
        parts.push(format!("{rev} ⚠ {n} warning{s} {rst}",
            rev = c.rev_yellow, rst = c.reset, n = warnings,
            s = if warnings == 1 { "" } else { "s" }));
    }
    format!("\n  {}\n\n", parts.join("  "))
}

/// Render informational notes with a quieter visual style.
pub fn render_notes_text(notes: &[Noted], color: bool) -> String {
    if notes.is_empty() { return String::new(); }
    let c = C::new(color);
    let mut src = SourceCache::default();
    let mut out = format!("  {rb} ℹ notes {rst}\n\n", rb = c.rev_green, rst = c.reset);
    for n in notes {
        let file = n.file.display().to_string();
        let line_w = digits(n.line).max(2);
        out += &format!("  {bold_cyan}·{rst} {dim}{}{rst}\n",
            n.note, bold_cyan = c.bold_cyan, dim = c.dim, rst = c.reset);
        out += &format!("    {dim}{file}:{}{rst}\n", n.line, dim = c.dim, rst = c.reset);
        if let Some(raw) = src.line(&n.file, n.line) {
            out += &format!("    {dim}{n:>w$}  │  {rst}{raw}\n",
                n = n.line, w = line_w, dim = c.dim, rst = c.reset);
        }
        out.push('\n');
    }
    out
}

fn expr_text(f: &Finding) -> String {
    if f.raw_op.is_empty() { f.raw_lhs.clone() }
    else { format!("{} {} {}", f.raw_lhs, f.raw_op, f.raw_rhs) }
}

fn digits(n: usize) -> usize {
    if n == 0 { 1 } else { n.ilog10() as usize + 1 }
}

// ── SARIF ─────────────────────────────────────────────────────────────────────

pub fn render_sarif(findings: &[Finding], notes: &[Noted]) -> serde_json::Value {
    let mut rule_ids: std::collections::BTreeSet<&str> =
        findings.iter().map(|f| f.kind.rule_id()).collect();
    if !notes.is_empty() { rule_ids.insert("note"); }
    let rules: Vec<serde_json::Value> = rule_ids
        .into_iter().map(|id| serde_json::json!({ "id": id })).collect();

    let mut results: Vec<serde_json::Value> = findings.iter().map(|f| {
        serde_json::json!({
            "ruleId": f.kind.rule_id(),
            "level": match f.severity { Severity::Error => "error", Severity::Warning => "warning" },
            "message": { "text": f.kind.to_string() },
            "locations": [{ "physicalLocation": {
                "artifactLocation": { "uri": f.file.to_string_lossy() },
                "region": { "startLine": f.line }
            }}]
        })
    }).collect();

    for n in notes {
        results.push(serde_json::json!({
            "ruleId": "note", "kind": "informational", "level": "note",
            "message": { "text": n.note.to_string() },
            "locations": [{ "physicalLocation": {
                "artifactLocation": { "uri": n.file.to_string_lossy() },
                "region": { "startLine": n.line }
            }}]
        }));
    }

    serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{ "tool": { "driver": {
            "name": "silicon", "version": env!("CARGO_PKG_VERSION"), "rules": rules
        }}, "results": results }]
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use checker::check;
    use fw_parse::extract_accesses;
    use std::path::Path;
    use svd_model::Model;

    fn rp2040_model() -> Model {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/rp2040.svd");
        let xml = std::fs::read_to_string(path).expect("vendored rp2040.svd must exist");
        Model::from_svd_str(&xml).expect("rp2040.svd must parse")
    }

    fn findings_for(src: &str) -> Vec<Finding> {
        let model = rp2040_model();
        let accesses = extract_accesses(src, Path::new("test.c"));
        check(&model, &accesses).findings
    }

    #[test]
    fn render_text_includes_severity_rule_and_title() {
        let findings = findings_for("void f(void) { clocks_hw->clk_gpout0_ctrl = 12u << 5; }");
        let text = render_text(&findings, &[], false);
        assert!(text.contains("error"), "must contain severity");
        assert!(text.contains("field-value-not-in-enum"), "must show rule id");
        assert!(text.contains("AUXSRC"), "title must name the field");
        assert!(text.contains("allowed:"), "citation must list allowed values");
        assert!(text.contains("clksrc_pll_sys=0"), "citation must list the first AUXSRC value");
    }

    #[test]
    fn render_text_no_color_has_no_escape_codes() {
        let findings = findings_for("void f(void) { clocks_hw->clk_gpout0_ctrl = 12u << 5; }");
        let text = render_text(&findings, &[], false);
        assert!(!text.contains('\x1b'), "no ANSI escapes without color");
    }

    #[test]
    fn render_text_color_has_escape_codes() {
        let findings = findings_for("void f(void) { clocks_hw->clk_gpout0_ctrl = 12u << 5; }");
        let text = render_text(&findings, &[], true);
        assert!(text.contains('\x1b'), "must contain ANSI escapes with color=true");
    }

    #[test]
    fn render_text_on_no_findings_says_no_findings() {
        let text = render_text(&[], &[], false);
        assert!(text.contains("no findings"));
    }

    #[test]
    fn render_sarif_has_matching_rule_and_result() {
        let findings = findings_for("void f(void) { clocks_hw->clk_gpout0_ctrl = 12u << 5; }");
        let sarif = render_sarif(&findings, &[]);
        assert_eq!(sarif["version"], "2.1.0");
        let rules = sarif["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["id"] == "field-value-not-in-enum"));
        let results = sarif["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["ruleId"], "field-value-not-in-enum");
        assert_eq!(results[0]["level"], "error");
        assert_eq!(results[0]["locations"][0]["physicalLocation"]["region"]["startLine"], 1);
    }

    #[test]
    fn render_sarif_maps_warning_severity() {
        let findings = findings_for("void f(void) { pll_sys_hw->fbdiv_int = 5000; }");
        assert_eq!(findings[0].kind.severity(), checker::Severity::Warning);
        let sarif = render_sarif(&findings, &[]);
        assert_eq!(sarif["runs"][0]["results"][0]["level"], "warning");
    }

    #[test]
    fn render_sarif_includes_notes_as_informational() {
        let model = rp2040_model();
        let accesses = extract_accesses(
            "void f(void) { pll_sys_hw->fbdiv_int = 100; }",
            Path::new("test.c"),
        );
        let result = check(&model, &accesses);
        assert!(!result.notes.is_empty());
        let sarif = render_sarif(&result.findings, &result.notes);
        let results = sarif["runs"][0]["results"].as_array().unwrap();
        let note_results: Vec<_> = results.iter().filter(|r| r["level"] == "note").collect();
        assert!(!note_results.is_empty());
        assert_eq!(note_results[0]["kind"], "informational");
        let rules = sarif["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["id"] == "note"));
    }
}
