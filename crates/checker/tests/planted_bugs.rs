//! M3 done-when criterion: on firmware with a planted invalid bitfield, the
//! checker flags exactly that line with the SVD citation, and produces ZERO
//! findings on the known-correct equivalent.

use checker::{check, FindingKind};
use fw_parse::extract_accesses;
use svd_model::Model;
use std::path::{Path, PathBuf};

fn rp2040_model() -> Model {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/rp2040.svd");
    let xml = std::fs::read_to_string(path).expect("vendored rp2040.svd must exist");
    Model::from_svd_str(&xml).expect("rp2040.svd must parse")
}

fn bench_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench").join(rel)
}

fn check_file(model: &Model, rel: &str) -> checker::CheckResult {
    let path = bench_path(rel);
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    let accesses = extract_accesses(&source, &path);
    check(model, &accesses)
}

#[test]
fn correct_clock_auxsrc_produces_zero_findings() {
    let model = rp2040_model();
    let result = check_file(&model, "correct/clock_gpout_auxsrc.c");
    assert!(
        result.findings.is_empty(),
        "expected zero findings on known-correct firmware, got {:?}",
        result.findings
    );
}

#[test]
fn hallucinated_clock_auxsrc_flags_exactly_that_line() {
    let model = rp2040_model();
    let result = check_file(&model, "hallucinated/clock_gpout_auxsrc_bad_enum.c");
    assert_eq!(
        result.findings.len(),
        1,
        "expected exactly one finding, got {:?}",
        result.findings
    );
    let f = &result.findings[0];
    assert_eq!(f.line, 14);
    match &f.kind {
        FindingKind::FieldValueNotInEnum { peripheral, register, field, value, allowed } => {
            assert_eq!(peripheral, "CLOCKS");
            assert_eq!(register, "CLK_GPOUT0_CTRL");
            assert_eq!(field, "AUXSRC");
            assert_eq!(*value, 12);
            assert_eq!(allowed.len(), 11);
            assert!(!allowed.iter().any(|v| v.value == 12));
        }
        other => panic!("expected FieldValueNotInEnum, got {other:?}"),
    }
}

#[test]
fn correct_pll_fbdiv_produces_zero_findings() {
    let model = rp2040_model();
    let result = check_file(&model, "correct/pll_fbdiv.c");
    assert!(
        result.findings.is_empty(),
        "expected zero findings on known-correct firmware, got {:?}",
        result.findings
    );
}

#[test]
fn hallucinated_pll_fbdiv_too_wide_flags_exactly_that_line() {
    let model = rp2040_model();
    let result = check_file(&model, "hallucinated/pll_fbdiv_too_wide.c");
    assert_eq!(
        result.findings.len(),
        1,
        "expected exactly one finding, got {:?}",
        result.findings
    );
    let f = &result.findings[0];
    assert_eq!(f.line, 16);
    match &f.kind {
        FindingKind::ValueSetsUndefinedBits { peripheral, register, value, defined_mask } => {
            assert_eq!(peripheral, "PLL_SYS");
            assert_eq!(register, "FBDIV_INT");
            assert_eq!(*value, 5000);
            assert_eq!(*defined_mask, 0xFFF);
        }
        other => panic!("expected ValueSetsUndefinedBits, got {other:?}"),
    }
}
