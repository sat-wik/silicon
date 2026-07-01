//! M4: renders checker findings as a terminal report or a SARIF document.
//!
//! Purely a presentation layer — every fact rendered here (severity, rule
//! id, citation) already exists on the `Finding`; this crate decides
//! nothing about correctness (CLAUDE.md invariant 1).

use checker::{Finding, Noted, Severity};

/// Human-readable terminal report: one block per finding, file/line,
/// offending expression, and the SVD-grounded plain-language explanation,
/// followed by a one-line summary count.
pub fn render_text(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        let level = match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        out.push_str(&format!(
            "{level}[{rule}] {file}:{line}\n  {lhs} {op} {rhs}\n  {explanation}\n\n",
            rule = f.kind.rule_id(),
            file = f.file.display(),
            line = f.line,
            lhs = f.raw_lhs,
            op = f.raw_op,
            rhs = f.raw_rhs,
            explanation = f.kind,
        ));
    }
    let errors = findings.iter().filter(|f| f.severity == Severity::Error).count();
    let warnings = findings.iter().filter(|f| f.severity == Severity::Warning).count();
    out.push_str(&format!("{errors} error(s), {warnings} warning(s)\n"));
    out
}

/// A minimal SARIF 2.1.0 document.
///
/// Findings emit as `error` or `warning` results. Notes emit as `note`-level
/// informational results so they appear in code-scanning UIs alongside findings
/// without causing CI to fail — the distinction between "violation" and
/// "unverifiable" stays visible without conflating the two.
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
                "level": match f.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                },
                "message": { "text": f.kind.to_string() },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": f.file.to_string_lossy() },
                        "region": { "startLine": f.line }
                    }
                }]
            })
        })
        .collect();

    for n in notes {
        results.push(serde_json::json!({
            "ruleId": "note",
            "kind": "informational",
            "level": "note",
            "message": { "text": n.note.to_string() },
            "locations": [{
                "physicalLocation": {
                    "artifactLocation": { "uri": n.file.to_string_lossy() },
                    "region": { "startLine": n.line }
                }
            }]
        }));
    }

    serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "silicon",
                    "version": env!("CARGO_PKG_VERSION"),
                    "rules": rules
                }
            },
            "results": results
        }]
    })
}

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
    fn render_text_includes_severity_rule_location_and_citation() {
        let findings = findings_for("void f(void) { clocks_hw->clk_gpout0_ctrl = 12u << 5; }");
        let text = render_text(&findings);
        assert!(text.contains("error[field-value-not-in-enum]"));
        assert!(text.contains("test.c:1"));
        assert!(text.contains("CLOCKS.CLK_GPOUT0_CTRL.AUXSRC"));
        assert!(text.contains("1 error(s), 0 warning(s)"));
    }

    #[test]
    fn render_text_on_no_findings_reports_zero() {
        let text = render_text(&[]);
        assert_eq!(text, "0 error(s), 0 warning(s)\n");
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
        let accesses = fw_parse::extract_accesses(
            "void f(void) { pll_sys_hw->fbdiv_int = 100; }",
            std::path::Path::new("test.c"),
        );
        let result = checker::check(&model, &accesses);
        assert!(!result.notes.is_empty(), "FBDIV_INT has no enum so must produce notes");
        let sarif = render_sarif(&result.findings, &result.notes);
        let results = sarif["runs"][0]["results"].as_array().unwrap();
        // Zero findings but at least one note result.
        assert!(result.findings.is_empty());
        let note_results: Vec<_> = results.iter().filter(|r| r["level"] == "note").collect();
        assert!(!note_results.is_empty());
        assert_eq!(note_results[0]["kind"], "informational");
        assert_eq!(note_results[0]["ruleId"], "note");
        // "note" rule must appear in the driver's rules list.
        let rules = sarif["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["id"] == "note"));
    }
}
