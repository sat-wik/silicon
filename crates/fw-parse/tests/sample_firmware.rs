//! M2 done-when criterion: running the extractor on a sample RP2040 file
//! lists every register access with resolved meaning, and clearly marks
//! the unresolved ones.

use fw_parse::{extract_accesses, AccessTier, Target};
use std::path::Path;

fn sample_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/correct/sample_pll_gpio.c")
}

#[test]
fn lists_every_access_and_marks_unresolved_ones() {
    let path = sample_path();
    let source = std::fs::read_to_string(&path).expect("sample firmware file must exist");
    let accesses = extract_accesses(&source, &path);

    for a in &accesses {
        let target_desc = match &a.target {
            Target::StructField { peripheral, register } => {
                format!("{peripheral}.{register} (struct field)")
            }
            Target::RegsMacro { peripheral, register: Some(r) } => {
                format!("{peripheral}.{r} (regs macro)")
            }
            Target::RegsMacro { peripheral, register: None } => {
                format!("{peripheral}.<unknown> (regs macro, base only)")
            }
            Target::Address(addr) => format!("0x{addr:08x} (literal address)"),
            Target::Unresolved => "UNRESOLVED".to_string(),
        };
        let value_desc = match a.value {
            Some(v) => format!("0x{v:x}"),
            None => "<not statically known>".to_string(),
        };
        println!(
            "{}:{}  [{:?}]  {} {:?} {}  => target={target_desc} value={value_desc}",
            a.file.display(),
            a.line,
            a.tier,
            a.raw_lhs,
            a.op,
            a.raw_rhs,
        );
    }

    // Every access in the sample is one of the three in-scope shapes; none
    // should be silently dropped, and the ones with no statically
    // determinable target must say so rather than be missing or guessed.
    assert_eq!(accesses.len(), 10, "expected exactly 10 recognized accesses in the sample file");

    let unresolved: Vec<_> = accesses.iter().filter(|a| a.target == Target::Unresolved).collect();
    assert_eq!(unresolved.len(), 2, "the two runtime-variable accesses must be marked unresolved");
    for u in &unresolved {
        assert!(u.tier == AccessTier::RawPointer || u.tier == AccessTier::StructField);
    }

    let resolved_count = accesses.len() - unresolved.len();
    assert_eq!(resolved_count, 8, "the other 8 accesses must resolve a peripheral/register name or address");

    // PLL_SYS_BASE and PLL_SYS_FBDIV_INT_OFFSET are both #define'd with numeric
    // literals in sample_pll_gpio.c, so macro folding now resolves the BASE+OFFSET
    // expression to a concrete address rather than a symbolic RegsMacro target —
    // more precise for the checker's address-lookup path.
    assert!(accesses.iter().any(|a| a.target == Target::Address(0x40028008)),
        "PLL_SYS_BASE+PLL_SYS_FBDIV_INT_OFFSET must fold to Address(0x40028008)");
    // Struct-tier access is still symbolically resolved as before.
    assert!(accesses.iter().any(|a| a.target
        == Target::StructField { peripheral: "PLL_SYS".into(), register: "FBDIV_INT".into() }));
    // Literal-only raw-pointer address still works.
    assert!(accesses.iter().any(|a| a.target == Target::Address(0x40028000)));
}
