//! M4: renders checker findings as a terminal report or a SARIF document.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use checker::{Finding, Noted, Severity};

// ── ANSI colour helpers ───────────────────────────────────────────────────────

struct C {
    reset: &'static str,
    bold: &'static str,
    dim: &'static str,
    bold_red: &'static str,
    bold_yellow: &'static str,
    bold_blue: &'static str,
    cyan: &'static str,
}

impl C {
    fn new(color: bool) -> Self {
        if color {
            C {
                reset: "\x1b[0m",
                bold: "\x1b[1m",
                dim: "\x1b[2m",
                bold_red: "\x1b[1;31m",
                bold_yellow: "\x1b[1;33m",
                bold_blue: "\x1b[1;34m",
                cyan: "\x1b[36m",
            }
        } else {
            C { reset: "", bold: "", dim: "", bold_red: "", bold_yellow: "", bold_blue: "", cyan: "" }
        }
    }
    fn severity(&self, s: Severity) -> &'static str {
        match s {
            Severity::Error => self.bold_red,
            Severity::Warning => self.bold_yellow,
        }
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

/// Human-readable terminal report with source excerpts, caret underlines, and
/// colour. Pass `color = false` when output is not a terminal (piped / file).
pub fn render_text(findings: &[Finding], _notes: &[Noted], color: bool) -> String {
    let c = C::new(color);
    let mut src = SourceCache::default();
    let mut out = String::new();

    for f in findings {
        out.push_str(&render_finding(f, &mut src, &c));
    }

    out.push_str(&render_summary(findings, &c));
    out
}

fn render_finding(f: &Finding, src: &mut SourceCache, c: &C) -> String {
    let mut out = String::new();
    let sev_str = match f.severity { Severity::Error => "error", Severity::Warning => "warning" };
    let sev_c = c.severity(f.severity);

    // ── headline ──────────────────────────────────────────────────────────────
    out += &format!(
        "{sev_c}{sev_str}[{}]{rst}: {bold}{}{rst}\n",
        f.kind.rule_id(), f.kind.title(),
        sev_c = sev_c, rst = c.reset, bold = c.bold,
    );

    // ── file / line pointer ───────────────────────────────────────────────────
    let file = f.file.display().to_string();
    let line_w = digits(f.line);
    out += &format!(" {blue}{:line_w$} --> {rst}{bold}{file}:{}{rst}\n", "", f.line,
        blue = c.bold_blue, rst = c.reset, bold = c.bold);
    out += &gutter(line_w, None, c);

    // ── source line + caret ───────────────────────────────────────────────────
    let expr = expr_text(f);
    if let Some(raw) = src.line(&f.file, f.line) {
        out += &gutter_line(f.line, line_w, raw, c);
        out += &caret_line(raw, &expr, line_w, c, f.severity);
    } else {
        // source file not readable: show the expression we extracted
        let synthetic = format!("    {expr}");
        out += &gutter_line(f.line, line_w, &synthetic, c);
        out += &caret_line(&synthetic, &expr, line_w, c, f.severity);
    }

    out += &gutter(line_w, None, c);

    // ── SVD citation ──────────────────────────────────────────────────────────
    if let Some(cit) = f.kind.citation() {
        for (i, line) in cit.lines().enumerate() {
            if i == 0 {
                out += &format!(" {blue}{:line_w$} = {rst}{cyan}{line}{rst}\n",
                    "", blue = c.bold_blue, rst = c.reset, cyan = c.cyan);
            } else {
                out += &format!(" {blue}{:line_w$}   {rst}{cyan}{line}{rst}\n",
                    "", blue = c.bold_blue, rst = c.reset, cyan = c.cyan);
            }
        }
    }

    out.push('\n');
    out
}

fn render_summary(findings: &[Finding], c: &C) -> String {
    let errors = findings.iter().filter(|f| f.severity == Severity::Error).count();
    let warnings = findings.iter().filter(|f| f.severity == Severity::Warning).count();
    if errors == 0 && warnings == 0 {
        return format!("{}{}{}\n", c.bold, "no findings", c.reset);
    }
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!("{}{} error{}{}", c.bold_red, errors,
            if errors == 1 { "" } else { "s" }, c.reset));
    }
    if warnings > 0 {
        parts.push(format!("{}{} warning{}{}", c.bold_yellow, warnings,
            if warnings == 1 { "" } else { "s" }, c.reset));
    }
    format!("{}found {}{}\n", c.bold, parts.join(" · "), c.reset)
}

/// Render informational notes in the same style as findings but quieter.
pub fn render_notes_text(notes: &[Noted], color: bool) -> String {
    if notes.is_empty() { return String::new(); }
    let c = C::new(color);
    let mut src = SourceCache::default();
    let mut out = format!("\n{}notes:{}\n", c.bold, c.reset);
    for n in notes {
        let file = n.file.display().to_string();
        let line_w = digits(n.line);
        out += &format!(" {blue}note:{rst} {dim}{}{rst}\n",
            n.note, blue = c.bold_blue, dim = c.dim, rst = c.reset);
        out += &format!("   {blue}-->{rst} {file}:{}\n",
            n.line, blue = c.bold_blue, rst = c.reset);
        if let Some(raw) = src.line(&n.file, n.line) {
            out += &gutter(line_w, None, &c);
            out += &gutter_line(n.line, line_w, raw, &c);
            out += &gutter(line_w, None, &c);
        }
        out.push('\n');
    }
    out
}

// ── Gutter helpers ────────────────────────────────────────────────────────────

fn gutter(line_w: usize, line_no: Option<usize>, c: &C) -> String {
    match line_no {
        None => format!(" {blue}{:line_w$} |{rst}\n", "", blue = c.bold_blue, rst = c.reset),
        Some(n) => format!(" {blue}{n:line_w$} |{rst}", blue = c.bold_blue, rst = c.reset),
    }
}

fn gutter_line(line_no: usize, line_w: usize, raw: &str, c: &C) -> String {
    format!("{} {raw}\n", gutter(line_w, Some(line_no), c))
}

fn caret_line(raw: &str, expr: &str, line_w: usize, c: &C, sev: Severity) -> String {
    let lead = raw.len() - raw.trim_start().len();
    let offset = raw.find(expr.trim_start()).unwrap_or(lead);
    let caret_len = expr.len().min(raw.len().saturating_sub(offset)).max(1);
    let carets = "^".repeat(caret_len);
    let spaces = " ".repeat(offset);
    format!(" {blue}{:line_w$} |{rst} {spaces}{sev_c}{carets}{rst}\n",
        "", blue = c.bold_blue, rst = c.reset, sev_c = c.severity(sev))
}

fn expr_text(f: &Finding) -> String {
    if f.raw_op.is_empty() {
        f.raw_lhs.clone()
    } else {
        format!("{} {} {}", f.raw_lhs, f.raw_op, f.raw_rhs)
    }
}

fn digits(n: usize) -> usize {
    if n == 0 { 1 } else { n.ilog10() as usize + 1 }
}

// ── SARIF ────────────────────────────────────────────────────────────────────

pub fn render_sarif(findings: &[Finding], notes: &[Noted]) -> serde_json::Value {
    let mut rule_ids: std::collections::BTreeSet<&str> =
        findings.iter().map(|f| f.kind.rule_id()).collect();
    if !notes.is_empty() {
        rule_ids.insert("note");
    }
    let rules: Vec<serde_json::Value> = rule_ids
        .into_iter()
        .map(|id| serde_json::json!({ "id": id }))
        .collect();

    let mut results: Vec<serde_json::Value> = findings
        .iter()
        .map(|f| {
            serde_json::json!({
                "ruleId": f.kind.rule_id(),
                "level": match f.severity { Severity::Error => "error", Severity::Warning => "warning" },
                "message": { "text": f.kind.to_string() },
                "locations": [{ "physicalLocation": {
                    "artifactLocation": { "uri": f.file.to_string_lossy() },
                    "region": { "startLine": f.line }
                }}]
            })
        })
        .collect();

    for n in notes {
        results.push(serde_json::json!({
            "ruleId": "note",
            "kind": "informational",
            "level": "note",
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
            "name": "silicon",
            "version": env!("CARGO_PKG_VERSION"),
            "rules": rules
        }}, "results": results }]
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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
        assert!(text.contains("error[field-value-not-in-enum]"), "must show rule id");
        assert!(text.contains("AUXSRC"), "title must name the field");
        assert!(text.contains("allowed:"), "citation must list allowed values");
        // sio_25 is a GPIO FUNCSEL value; the CLOCKS AUXSRC enum has different names
        assert!(text.contains("clksrc_pll_sys=0"), "citation must list the first AUXSRC value");
        assert!(text.contains("clksrc_pll_sys=0"), "citation must list first allowed value");
        assert!(text.contains("found 1 error"), "summary must count errors");
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
        assert_eq!(note_results[0]["ruleId"], "note");
        let rules = sarif["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["id"] == "note"));
    }
}
