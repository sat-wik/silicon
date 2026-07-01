//! M4 done-when criterion, exercised end-to-end through the actual binary:
//! a firmware file with a planted register bug fails (non-zero exit, a
//! cited finding), and the known-correct equivalent passes (exit 0, zero
//! findings).

use std::path::Path;
use std::process::Command;

fn bench_path(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("bench").join(rel)
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_silicon"))
        .args(args)
        .output()
        .expect("failed to run silicon binary")
}

/// On failure, prints stdout/stderr so a CI log shows *why* — e.g. the
/// actual cited finding — instead of just "assertion failed".
fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_failure(output: &std::process::Output) {
    assert!(
        !output.status.success(),
        "expected a non-zero exit, but the process succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn correct_firmware_exits_zero_with_zero_findings() {
    let path = bench_path("correct");
    let output = run(&[path.to_str().unwrap()]);
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("no findings"));
}

#[test]
fn hallucinated_firmware_exits_nonzero_with_cited_finding() {
    let path = bench_path("hallucinated/clock_gpout_auxsrc_bad_enum.c");
    let output = run(&[path.to_str().unwrap()]);
    assert_failure(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[field-value-not-in-enum]"));
    assert!(stdout.contains("CLOCKS.CLK_GPOUT0_CTRL.AUXSRC"));
    assert!(stdout.contains(":14"));
}

#[test]
fn sarif_format_is_valid_json_with_a_result() {
    let path = bench_path("hallucinated/clock_gpout_auxsrc_bad_enum.c");
    let output = run(&["--format", "sarif", path.to_str().unwrap()]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert_eq!(json["version"], "2.1.0");
    let results = json["runs"][0]["results"].as_array().unwrap();
    // SARIF now includes both findings (error/warning) and notes (informational).
    let error_results: Vec<_> = results.iter().filter(|r| r["level"] == "error").collect();
    assert_eq!(error_results.len(), 1, "expected exactly one error-level result");
}

#[test]
fn fail_on_none_always_exits_zero() {
    let path = bench_path("hallucinated");
    let output = run(&["--fail-on", "none", path.to_str().unwrap()]);
    assert_success(&output);
}

#[test]
fn warning_only_finding_does_not_fail_default_error_threshold() {
    let path = bench_path("hallucinated/pll_fbdiv_too_wide.c");
    let output = run(&[path.to_str().unwrap()]);
    // This file's only finding is a Warning (value-sets-undefined-bits);
    // the default --fail-on error must not trip on it.
    assert_success(&output);
}

#[test]
fn fail_on_warning_does_fail_on_warning_only_finding() {
    let path = bench_path("hallucinated/pll_fbdiv_too_wide.c");
    let output = run(&["--fail-on", "warning", path.to_str().unwrap()]);
    assert_failure(&output);
}
