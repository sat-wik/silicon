//! M3: resolves register accesses against the SVD model and emits findings.
//!
//! This is the core: the only crate that decides whether something is a
//! violation, and every decision must be traceable to a parsed SVD fact
//! (CLAUDE.md invariant 1). When a target or value isn't statically
//! determinable, or a field has no SVD enum, that produces a [`Note`]
//! (informational), never a [`Finding`] — see invariants 2 and 6.

use std::path::PathBuf;

use fw_parse::{AssignOp, RegisterAccess, Target};
use svd_model::{Access, AddressLookup, EnumValue, FieldModel, Model, PeripheralModel, RegisterModel};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingKind {
    /// The peripheral exists in the SVD but no register by this name does.
    NonexistentRegister { peripheral: String, register: String },
    /// A literal address falls inside a known peripheral's address block
    /// but doesn't match any defined register's start address.
    AddressNotARegister { peripheral: String, address: u64 },
    /// The written value doesn't fit in the register's bit width at all.
    ValueExceedsRegisterWidth {
        peripheral: String,
        register: String,
        value: u64,
        size_bits: u32,
    },
    /// The written value sets bits the SVD doesn't assign to any field —
    /// reserved bits, or (for a register with exactly one undersized field)
    /// a value too wide for that field.
    ValueSetsUndefinedBits {
        peripheral: String,
        register: String,
        value: u64,
        defined_mask: u64,
    },
    /// A field's resulting value isn't one of the SVD's enumerated values.
    FieldValueNotInEnum {
        peripheral: String,
        register: String,
        field: String,
        value: u64,
        allowed: Vec<EnumValue>,
    },
    /// The whole register is read-only in the SVD, but firmware writes it.
    WriteToReadOnlyRegister { peripheral: String, register: String },
    /// A specific field is read-only, but firmware sets bits within it.
    WriteToReadOnlyField {
        peripheral: String,
        register: String,
        field: String,
    },
}

/// How confident a finding is. Every variant is already conservative (see
/// CLAUDE.md invariant 2 — uncertain cases are `Note`s, not findings at any
/// severity), but some violation classes are unambiguous bugs (`Error`)
/// while others rest on a CMSIS access annotation whose real-world effect
/// the spec itself calls "undefined" (`Warning`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

impl FindingKind {
    pub fn severity(&self) -> Severity {
        match self {
            FindingKind::NonexistentRegister { .. }
            | FindingKind::AddressNotARegister { .. }
            | FindingKind::ValueExceedsRegisterWidth { .. }
            | FindingKind::FieldValueNotInEnum { .. }
            | FindingKind::WriteToReadOnlyRegister { .. } => Severity::Error,
            FindingKind::ValueSetsUndefinedBits { .. } | FindingKind::WriteToReadOnlyField { .. } => {
                Severity::Warning
            }
        }
    }

    /// Stable identifier for this violation class — used as the SARIF rule
    /// id. Never changes meaning for a given variant; safe to key tooling on.
    pub fn rule_id(&self) -> &'static str {
        match self {
            FindingKind::NonexistentRegister { .. } => "nonexistent-register",
            FindingKind::AddressNotARegister { .. } => "address-not-a-register",
            FindingKind::ValueExceedsRegisterWidth { .. } => "value-exceeds-register-width",
            FindingKind::ValueSetsUndefinedBits { .. } => "value-sets-undefined-bits",
            FindingKind::FieldValueNotInEnum { .. } => "field-value-not-in-enum",
            FindingKind::WriteToReadOnlyRegister { .. } => "write-to-read-only-register",
            FindingKind::WriteToReadOnlyField { .. } => "write-to-read-only-field",
        }
    }
}

impl std::fmt::Display for FindingKind {
    /// A plain-language explanation that embeds the SVD citation backing
    /// it (peripheral/register/field, allowed values, address) — every
    /// finding must be traceable to a parsed SVD fact, so the explanation
    /// and the citation are the same data, not two separate stories.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingKind::NonexistentRegister { peripheral, register } => write!(
                f,
                "{peripheral}.{register} is not a register defined in the SVD for {peripheral}"
            ),
            FindingKind::AddressNotARegister { peripheral, address } => write!(
                f,
                "address 0x{address:08x} falls inside {peripheral}'s address block but matches no register's start address in the SVD"
            ),
            FindingKind::ValueExceedsRegisterWidth { peripheral, register, value, size_bits } => write!(
                f,
                "value 0x{value:x} written to {peripheral}.{register} does not fit in its {size_bits}-bit width"
            ),
            FindingKind::ValueSetsUndefinedBits { peripheral, register, value, defined_mask } => write!(
                f,
                "value 0x{value:x} written to {peripheral}.{register} sets bits outside the fields the SVD defines for it (defined bits: 0x{defined_mask:x})"
            ),
            FindingKind::FieldValueNotInEnum { peripheral, register, field, value, allowed } => {
                let allowed_str = allowed
                    .iter()
                    .map(|v| format!("{}={}", v.name, v.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "value {value} written to {peripheral}.{register}.{field} is not one of the SVD's allowed values: {allowed_str}"
                )
            }
            FindingKind::WriteToReadOnlyRegister { peripheral, register } => write!(
                f,
                "{peripheral}.{register} is read-only in the SVD, but firmware writes to it"
            ),
            FindingKind::WriteToReadOnlyField { peripheral, register, field } => write!(
                f,
                "{peripheral}.{register}.{field} is read-only in the SVD, but firmware sets bits within it"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    pub file: PathBuf,
    pub line: usize,
    pub raw_lhs: String,
    pub raw_op: &'static str,
    pub raw_rhs: String,
}

/// Informational observations: true per CLAUDE.md invariant 6, but not
/// flaggable as violations because something necessary to judge them isn't
/// statically known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// fw-parse recognized the access shape but couldn't determine a target.
    UnresolvedAccess,
    /// A struct/macro name was guessed but no SVD peripheral matches it —
    /// likely not actually a register access, not a confirmed bug.
    UnknownPeripheralGuess { peripheral: String },
    /// Only the peripheral was determined; no register name was available.
    PeripheralOnlyKnown { peripheral: String },
    /// A literal address doesn't fall within any peripheral's address block.
    AddressNotMapped { address: u64 },
    /// A field has no SVD enum: value-membership is unverifiable, not permissive.
    FieldUnverifiableNoEnum {
        peripheral: String,
        register: String,
        field: String,
    },
    /// A register has no fields modeled at all: bit-level checks are skipped.
    RegisterHasNoFields { peripheral: String, register: String },
    /// The op (e.g. `&=`, `+=`, a shift) doesn't have a statically
    /// determinable resulting value without knowing the prior register
    /// contents, so value-dependent checks were skipped for this access.
    ValueNotDeterminableForOp { peripheral: String, register: String },
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Note::UnresolvedAccess => write!(f, "access target is not statically determinable"),
            Note::UnknownPeripheralGuess { peripheral } => {
                write!(f, "no SVD peripheral named {peripheral}; likely not a register access")
            }
            Note::PeripheralOnlyKnown { peripheral } => {
                write!(f, "peripheral {peripheral} resolved, but no register name was determinable")
            }
            Note::AddressNotMapped { address } => {
                write!(f, "address 0x{address:08x} is not within any peripheral's address block")
            }
            Note::FieldUnverifiableNoEnum { peripheral, register, field } => write!(
                f,
                "{peripheral}.{register}.{field} has no enumerated values in the SVD; value membership is unverifiable"
            ),
            Note::RegisterHasNoFields { peripheral, register } => {
                write!(f, "{peripheral}.{register} has no fields defined in the SVD; bit-level checks skipped")
            }
            Note::ValueNotDeterminableForOp { peripheral, register } => write!(
                f,
                "the resulting value written to {peripheral}.{register} isn't statically determinable for this operator"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Noted {
    pub note: Note,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CheckResult {
    pub findings: Vec<Finding>,
    pub notes: Vec<Noted>,
}

pub fn check(model: &Model, accesses: &[RegisterAccess]) -> CheckResult {
    let mut result = CheckResult::default();
    for access in accesses {
        check_one(model, access, &mut result);
    }
    result
}

fn check_one(model: &Model, access: &RegisterAccess, result: &mut CheckResult) {
    let (peripheral, register) = match &access.target {
        Target::Unresolved => return note(result, access, Note::UnresolvedAccess),
        Target::RegsMacro { peripheral, register: None } => {
            return note(result, access, Note::PeripheralOnlyKnown { peripheral: peripheral.clone() })
        }
        Target::StructField { peripheral, register }
        | Target::RegsMacro { peripheral, register: Some(register) } => {
            match resolve_named(model, peripheral, register) {
                Resolved::Ok(p, r) => (p, r),
                Resolved::UnknownPeripheral => {
                    return note(result, access, Note::UnknownPeripheralGuess { peripheral: peripheral.clone() })
                }
                Resolved::NonexistentRegister => {
                    return finding(
                        result,
                        access,
                        FindingKind::NonexistentRegister {
                            peripheral: peripheral.clone(),
                            register: register.clone(),
                        },
                    )
                }
            }
        }
        Target::Address(addr) => match model.resolve_address(*addr) {
            AddressLookup::Register(p, r) => (p, r),
            AddressLookup::WithinPeripheral(p) => {
                return finding(
                    result,
                    access,
                    FindingKind::AddressNotARegister {
                        peripheral: p.name.clone(),
                        address: *addr,
                    },
                )
            }
            AddressLookup::Unmapped => return note(result, access, Note::AddressNotMapped { address: *addr }),
        },
    };

    check_register(peripheral, register, access, result);
}

enum Resolved<'m> {
    Ok(&'m PeripheralModel, &'m RegisterModel),
    UnknownPeripheral,
    NonexistentRegister,
}

fn resolve_named<'m>(model: &'m Model, peripheral: &str, register: &str) -> Resolved<'m> {
    let Some(p) = model.peripheral(peripheral) else {
        return Resolved::UnknownPeripheral;
    };
    let Some(r) = p.registers.iter().find(|r| r.name == register) else {
        return Resolved::NonexistentRegister;
    };
    Resolved::Ok(p, r)
}

fn check_register(peripheral: &PeripheralModel, register: &RegisterModel, access: &RegisterAccess, result: &mut CheckResult) {
    if register.access == Access::ReadOnly {
        finding(
            result,
            access,
            FindingKind::WriteToReadOnlyRegister {
                peripheral: peripheral.name.clone(),
                register: register.name.clone(),
            },
        );
    }

    let Some(value) = access.value else { return };

    // Only `=` and `|=` have a value-effect on the resulting bits that's
    // determinable without knowing the register's prior contents: `=`
    // replaces everything, `|=` unconditionally forces its 1-bits high.
    // `&=`, `^=`, and arithmetic/shift ops depend on prior state — skip
    // value-dependent checks for those rather than guess.
    if !matches!(access.op, AssignOp::Assign | AssignOp::OrAssign) {
        return note(
            result,
            access,
            Note::ValueNotDeterminableForOp {
                peripheral: peripheral.name.clone(),
                register: register.name.clone(),
            },
        );
    }

    if register.size_bits < 64 && value >= (1u64 << register.size_bits) {
        finding(
            result,
            access,
            FindingKind::ValueExceedsRegisterWidth {
                peripheral: peripheral.name.clone(),
                register: register.name.clone(),
                value,
                size_bits: register.size_bits,
            },
        );
    }

    if register.fields.is_empty() {
        return note(
            result,
            access,
            Note::RegisterHasNoFields {
                peripheral: peripheral.name.clone(),
                register: register.name.clone(),
            },
        );
    }

    let defined_mask: u64 = register.fields.iter().map(field_mask).fold(0, |a, m| a | m);
    if value & !defined_mask != 0 {
        finding(
            result,
            access,
            FindingKind::ValueSetsUndefinedBits {
                peripheral: peripheral.name.clone(),
                register: register.name.clone(),
                value,
                defined_mask,
            },
        );
    }

    for field in &register.fields {
        check_field(peripheral, register, field, access, value, result);
    }
}

fn check_field(
    peripheral: &PeripheralModel,
    register: &RegisterModel,
    field: &FieldModel,
    access: &RegisterAccess,
    value: u64,
    result: &mut CheckResult,
) {
    let mask = field_mask(field);

    // For `|=`, only fields fully covered by the OR'd-in 1-bits have a
    // determinable resulting value; partial overlap depends on prior state.
    let determinable = match access.op {
        AssignOp::Assign => true,
        AssignOp::OrAssign => value & mask == mask,
        _ => unreachable!("non-value-determinable ops already returned above"),
    };

    if field.access == Access::ReadOnly && value & mask != 0 {
        finding(
            result,
            access,
            FindingKind::WriteToReadOnlyField {
                peripheral: peripheral.name.clone(),
                register: register.name.clone(),
                field: field.name.clone(),
            },
        );
    }

    if !determinable {
        return;
    }
    let field_value = (value >> field.bit_offset) & width_mask(field.bit_width);

    match &field.allowed_values {
        Some(allowed) => {
            if !allowed.iter().any(|v| v.value == field_value) {
                finding(
                    result,
                    access,
                    FindingKind::FieldValueNotInEnum {
                        peripheral: peripheral.name.clone(),
                        register: register.name.clone(),
                        field: field.name.clone(),
                        value: field_value,
                        allowed: allowed.clone(),
                    },
                );
            }
        }
        None => note(
            result,
            access,
            Note::FieldUnverifiableNoEnum {
                peripheral: peripheral.name.clone(),
                register: register.name.clone(),
                field: field.name.clone(),
            },
        ),
    }
}

fn width_mask(bit_width: u32) -> u64 {
    if bit_width >= 64 {
        u64::MAX
    } else {
        (1u64 << bit_width) - 1
    }
}

fn field_mask(field: &FieldModel) -> u64 {
    width_mask(field.bit_width) << field.bit_offset
}

fn finding(result: &mut CheckResult, access: &RegisterAccess, kind: FindingKind) {
    let severity = kind.severity();
    result.findings.push(Finding {
        kind,
        severity,
        file: access.file.clone(),
        line: access.line,
        raw_lhs: access.raw_lhs.clone(),
        raw_op: op_text(access.op),
        raw_rhs: access.raw_rhs.clone(),
    });
}

fn op_text(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::OrAssign => "|=",
        AssignOp::AndAssign => "&=",
        AssignOp::XorAssign => "^=",
        AssignOp::AddAssign => "+=",
        AssignOp::SubAssign => "-=",
        AssignOp::MulAssign => "*=",
        AssignOp::DivAssign => "/=",
        AssignOp::ModAssign => "%=",
        AssignOp::ShlAssign => "<<=",
        AssignOp::ShrAssign => ">>=",
    }
}

fn note(result: &mut CheckResult, access: &RegisterAccess, note: Note) {
    result.notes.push(Noted {
        note,
        file: access.file.clone(),
        line: access.line,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn rp2040_model() -> Model {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/rp2040.svd");
        let xml = std::fs::read_to_string(path).expect("vendored rp2040.svd must exist");
        Model::from_svd_str(&xml).expect("rp2040.svd must parse")
    }

    fn check_src(model: &Model, src: &str) -> CheckResult {
        let accesses = fw_parse::extract_accesses(src, Path::new("test.c"));
        check(model, &accesses)
    }

    #[test]
    fn write_to_read_only_field_is_flagged() {
        let model = rp2040_model();
        // SIO.CPUID's only field is read-only across the whole register.
        let result = check_src(&model, "void f(void) { sio_hw->cpuid = 5; }");
        assert_eq!(result.findings.len(), 1);
        match &result.findings[0].kind {
            FindingKind::WriteToReadOnlyField { peripheral, register, field } => {
                assert_eq!(peripheral, "SIO");
                assert_eq!(register, "CPUID");
                assert_eq!(field, "CPUID");
            }
            other => panic!("expected WriteToReadOnlyField, got {other:?}"),
        }
    }

    #[test]
    fn nonexistent_register_on_real_peripheral_is_flagged() {
        let model = rp2040_model();
        let result = check_src(&model, "void f(void) { sio_hw->not_a_real_register = 1; }");
        assert_eq!(result.findings.len(), 1);
        match &result.findings[0].kind {
            FindingKind::NonexistentRegister { peripheral, register } => {
                assert_eq!(peripheral, "SIO");
                assert_eq!(register, "NOT_A_REAL_REGISTER");
            }
            other => panic!("expected NonexistentRegister, got {other:?}"),
        }
    }

    #[test]
    fn literal_address_inside_peripheral_but_not_a_register_is_flagged() {
        let model = rp2040_model();
        // SIO base 0xd0000000; this offset doesn't align to any register.
        let result = check_src(&model, "void f(void) { *(volatile uint32_t *)(0xd0000001u) = 1; }");
        assert_eq!(result.findings.len(), 1);
        match &result.findings[0].kind {
            FindingKind::AddressNotARegister { peripheral, address } => {
                assert_eq!(peripheral, "SIO");
                assert_eq!(*address, 0xd0000001);
            }
            other => panic!("expected AddressNotARegister, got {other:?}"),
        }
    }

    #[test]
    fn literal_address_outside_any_peripheral_is_a_note_not_a_finding() {
        let model = rp2040_model();
        let result = check_src(&model, "void f(void) { *(volatile uint32_t *)(0xffffffffu) = 1; }");
        assert!(result.findings.is_empty());
        assert!(result
            .notes
            .iter()
            .any(|n| matches!(n.note, Note::AddressNotMapped { address: 0xffffffff })));
    }

    #[test]
    fn unknown_peripheral_guess_is_a_note_not_a_finding() {
        let model = rp2040_model();
        let result = check_src(&model, "void f(void) { my_custom_hw->thing = 1; }");
        assert!(result.findings.is_empty());
        assert!(result
            .notes
            .iter()
            .any(|n| matches!(&n.note, Note::UnknownPeripheralGuess { peripheral } if peripheral == "MY_CUSTOM")));
    }

    #[test]
    fn unresolved_target_is_a_note_not_a_finding() {
        let model = rp2040_model();
        let result = check_src(&model, "void f(volatile uint32_t *reg) { *reg = 5; }");
        assert!(result.findings.is_empty());
        assert!(result.notes.iter().any(|n| n.note == Note::UnresolvedAccess));
    }

    #[test]
    fn and_assign_skips_value_dependent_checks() {
        let model = rp2040_model();
        // Clearing bits via `&=` never sets a new bit, so even a literal
        // operand with "undefined" bits set must not be flagged.
        let result = check_src(&model, "void f(void) { pll_sys_hw->fbdiv_int &= 0xFFFFFFFFu; }");
        assert!(result.findings.is_empty());
        assert!(result.notes.iter().any(|n| matches!(
            &n.note,
            Note::ValueNotDeterminableForOp { peripheral, register }
                if peripheral == "PLL_SYS" && register == "FBDIV_INT"
        )));
    }

    #[test]
    fn field_with_no_enum_is_a_note_not_a_finding() {
        let model = rp2040_model();
        // FBDIV_INT has no SVD enum at all: membership is unverifiable.
        let result = check_src(&model, "void f(void) { pll_sys_hw->fbdiv_int = 100; }");
        assert!(result.findings.is_empty());
        assert!(result.notes.iter().any(|n| matches!(
            &n.note,
            Note::FieldUnverifiableNoEnum { peripheral, register, field }
                if peripheral == "PLL_SYS" && register == "FBDIV_INT" && field == "FBDIV_INT"
        )));
    }

    #[test]
    fn finding_kind_display_cites_svd_facts() {
        let model = rp2040_model();
        let result = check_src(&model, "void f(void) { clocks_hw->clk_gpout0_ctrl = 12u << 5; }");
        let text = result.findings[0].kind.to_string();
        assert!(text.contains("CLOCKS.CLK_GPOUT0_CTRL.AUXSRC"));
        assert!(text.contains("clksrc_pll_sys=0"));
        assert_eq!(result.findings[0].kind.rule_id(), "field-value-not-in-enum");
    }
}
