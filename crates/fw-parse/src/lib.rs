//! M2: parses RP2040 firmware C/C++ into a list of register accesses.
//!
//! This crate only extracts and best-effort *names* what an assignment
//! touches (by source-level naming convention: `<peripheral>_hw->member`,
//! `<PERIPHERAL>_BASE + <PERIPHERAL>_<REGISTER>_OFFSET`, or a constant-folded
//! literal address). It never decides whether that target is valid — it has
//! no SVD model. Anything not statically determinable is tagged
//! `Target::Unresolved` rather than guessed; resolving against ground truth
//! is the checker crate's job (M3), per CLAUDE.md invariant 1.

use std::path::{Path, PathBuf};

use tree_sitter::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    OrAssign,
    AndAssign,
    XorAssign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    ShlAssign,
    ShrAssign,
}

impl AssignOp {
    fn from_str(s: &str) -> Option<AssignOp> {
        Some(match s {
            "=" => AssignOp::Assign,
            "|=" => AssignOp::OrAssign,
            "&=" => AssignOp::AndAssign,
            "^=" => AssignOp::XorAssign,
            "+=" => AssignOp::AddAssign,
            "-=" => AssignOp::SubAssign,
            "*=" => AssignOp::MulAssign,
            "/=" => AssignOp::DivAssign,
            "%=" => AssignOp::ModAssign,
            "<<=" => AssignOp::ShlAssign,
            ">>=" => AssignOp::ShrAssign,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessTier {
    /// `*(volatile T *)(EXPR) = v` — raw memory-mapped pointer write.
    RawPointer,
    /// `peripheral_hw->member = v` — `hardware_structs` field access.
    StructField,
    /// Address built from `hardware/regs` `_BASE`/`_OFFSET` macro identifiers.
    RegsConstant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Peripheral + register guessed from the `<peripheral>_hw->member` naming convention.
    StructField { peripheral: String, register: String },
    /// Peripheral (+ register, if the offset macro followed the naming convention)
    /// guessed from `hardware/regs` macro identifiers.
    RegsMacro {
        peripheral: String,
        register: Option<String>,
    },
    /// A concrete address, known only because the expression was foldable
    /// literal arithmetic — no symbolic peripheral/register name available.
    Address(u64),
    /// Recognized tier/shape, but the concrete name or address isn't
    /// statically determinable (e.g. a runtime variable). Not a guess.
    Unresolved,
}

#[derive(Debug, Clone)]
pub struct RegisterAccess {
    pub tier: AccessTier,
    pub target: Target,
    pub op: AssignOp,
    /// The value being written, if the right-hand side is foldable constant
    /// integer arithmetic. `None` means not statically known — do not guess.
    pub value: Option<u64>,
    pub raw_lhs: String,
    pub raw_rhs: String,
    pub file: PathBuf,
    pub line: usize,
}

/// Walks `source` for assignment expressions and extracts every recognized
/// register-access shape. Anything that doesn't match one of the three
/// in-scope shapes (raw-pointer dereference write, `->` struct-field write,
/// or a foldable `_BASE`/`_OFFSET` macro address) is simply not emitted —
/// it isn't a register access this tool claims to understand, not a finding.
pub fn extract_accesses(source: &str, file: &Path) -> Vec<RegisterAccess> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("tree-sitter-c grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let bytes = source.as_bytes();
    walk(tree.root_node(), bytes, file, &mut out);
    out
}

fn walk(node: Node, src: &[u8], file: &Path, out: &mut Vec<RegisterAccess>) {
    if node.kind() == "assignment_expression" {
        if let Some(access) = resolve_assignment(node, src, file) {
            out.push(access);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, out);
    }
}

fn resolve_assignment(node: Node, src: &[u8], file: &Path) -> Option<RegisterAccess> {
    let op_node = node.child_by_field_name("operator")?;
    let op = AssignOp::from_str(text(op_node, src))?;
    let left = node.child_by_field_name("left")?;
    let right = node.child_by_field_name("right")?;

    let (tier, target) = resolve_lhs(left, src)?;
    let value = fold_const(right, src);
    let line = node.start_position().row + 1;

    Some(RegisterAccess {
        tier,
        target,
        op,
        value,
        raw_lhs: text(left, src).to_string(),
        raw_rhs: text(right, src).to_string(),
        file: file.to_path_buf(),
        line,
    })
}

/// Recognizes the two in-scope LHS shapes. Returns `None` if `left` isn't
/// one of them at all (e.g. a plain local variable) — that's "not our
/// pattern", distinct from `Target::Unresolved` ("our pattern, unknown target").
fn resolve_lhs<'a>(left: Node<'a>, src: &'a [u8]) -> Option<(AccessTier, Target)> {
    match left.kind() {
        "pointer_expression" => {
            if text(left.child_by_field_name("operator")?, src) != "*" {
                return None;
            }
            let argument = left.child_by_field_name("argument")?;
            let inner = unwrap_cast_and_parens(argument, src);
            Some(resolve_address_expr(inner, src))
        }
        "field_expression" => {
            if text(left.child_by_field_name("operator")?, src) != "->" {
                return None;
            }
            let struct_var = text(left.child_by_field_name("argument")?, src);
            let member = text(left.child_by_field_name("field")?, src);
            match peripheral_from_hw_var(struct_var) {
                Some(peripheral) => Some((
                    AccessTier::StructField,
                    Target::StructField {
                        peripheral,
                        register: member.to_uppercase(),
                    },
                )),
                None => Some((AccessTier::StructField, Target::Unresolved)),
            }
        }
        _ => None,
    }
}

/// Strips a `(cast_expression)` and any parens to get to the address
/// expression a pointer dereference is actually built from.
fn unwrap_cast_and_parens<'a>(mut node: Node<'a>, src: &'a [u8]) -> Node<'a> {
    loop {
        node = match node.kind() {
            "cast_expression" => match node.child_by_field_name("value") {
                Some(v) => v,
                None => return node,
            },
            "parenthesized_expression" => match node.named_child(0) {
                Some(v) => v,
                None => return node,
            },
            _ => return node,
        };
        let _ = src;
    }
}

fn resolve_address_expr(node: Node, src: &[u8]) -> (AccessTier, Target) {
    // Pure literal arithmetic: a concrete address, no symbolic name.
    if let Some(addr) = fold_const(node, src) {
        return (AccessTier::RawPointer, Target::Address(addr));
    }

    // `<PERIPH>_BASE + <PERIPH>_<REG>_OFFSET` (either order) or a bare macro.
    if node.kind() == "binary_expression" {
        let op = node.child_by_field_name("operator").map(|n| text(n, src));
        if matches!(op, Some("+") | Some("-")) {
            if let (Some(l), Some(r)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) {
                if l.kind() == "identifier" && r.kind() == "identifier" {
                    return (
                        AccessTier::RegsConstant,
                        resolve_base_offset_macros(text(l, src), text(r, src)),
                    );
                }
            }
        }
    } else if node.kind() == "identifier" {
        let name = text(node, src);
        if let Some(peripheral) = peripheral_from_base_macro(name) {
            return (
                AccessTier::RegsConstant,
                Target::RegsMacro {
                    peripheral,
                    register: None,
                },
            );
        }
    }

    (AccessTier::RawPointer, Target::Unresolved)
}

fn resolve_base_offset_macros(a: &str, b: &str) -> Target {
    let (base_macro, offset_macro) = if a.ends_with("_BASE") {
        (a, b)
    } else if b.ends_with("_BASE") {
        (b, a)
    } else {
        (a, b)
    };
    match peripheral_from_base_macro(base_macro) {
        Some(peripheral) => {
            let register = register_from_offset_macro(offset_macro, &peripheral);
            Target::RegsMacro {
                peripheral,
                register,
            }
        }
        None => Target::Unresolved,
    }
}

fn peripheral_from_hw_var(var: &str) -> Option<String> {
    var.strip_suffix("_hw").map(|s| s.to_uppercase())
}

fn peripheral_from_base_macro(name: &str) -> Option<String> {
    name.strip_suffix("_BASE").map(|s| s.to_string())
}

fn register_from_offset_macro(offset_macro: &str, peripheral: &str) -> Option<String> {
    let stripped = offset_macro.strip_suffix("_OFFSET")?;
    let prefix = format!("{peripheral}_");
    Some(
        stripped
            .strip_prefix(prefix.as_str())
            .unwrap_or(stripped)
            .to_string(),
    )
}

/// Constant-folds pure integer-literal arithmetic. Any identifier, call, or
/// other non-literal subexpression makes the whole thing unfoldable —
/// returns `None` rather than guessing a macro's value.
fn fold_const(node: Node, src: &[u8]) -> Option<u64> {
    match node.kind() {
        "number_literal" => parse_number_literal(text(node, src)),
        "parenthesized_expression" => fold_const(node.named_child(0)?, src),
        "unary_expression" => {
            let op = text(node.child_by_field_name("operator")?, src);
            let v = fold_const(node.child_by_field_name("argument")?, src)?;
            match op {
                "-" => Some(v.wrapping_neg()),
                "~" => Some(!v),
                "+" => Some(v),
                _ => None,
            }
        }
        "binary_expression" => {
            let op = text(node.child_by_field_name("operator")?, src);
            let l = fold_const(node.child_by_field_name("left")?, src)?;
            let r = fold_const(node.child_by_field_name("right")?, src)?;
            match op {
                "+" => Some(l.wrapping_add(r)),
                "-" => Some(l.wrapping_sub(r)),
                "*" => Some(l.wrapping_mul(r)),
                "/" => (r != 0).then(|| l / r),
                "%" => (r != 0).then(|| l % r),
                "|" => Some(l | r),
                "&" => Some(l & r),
                "^" => Some(l ^ r),
                "<<" => Some(l.wrapping_shl(r as u32)),
                ">>" => Some(l.wrapping_shr(r as u32)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_number_literal(text: &str) -> Option<u64> {
    let trimmed = text.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = trimmed.strip_prefix("0b").or_else(|| trimmed.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2).ok()
    } else {
        trimmed.parse::<u64>().ok()
    }
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn extract(src: &str) -> Vec<RegisterAccess> {
        extract_accesses(src, Path::new("test.c"))
    }

    #[test]
    fn raw_pointer_with_regs_macros_resolves_peripheral_and_register() {
        let accesses = extract(
            "void f(void) { *(volatile uint32_t *)(PLL_SYS_BASE + PLL_SYS_FBDIV_INT_OFFSET) = 100; }",
        );
        assert_eq!(accesses.len(), 1);
        let a = &accesses[0];
        assert_eq!(a.tier, AccessTier::RegsConstant);
        assert_eq!(
            a.target,
            Target::RegsMacro {
                peripheral: "PLL_SYS".to_string(),
                register: Some("FBDIV_INT".to_string())
            }
        );
        assert_eq!(a.value, Some(100));
        assert_eq!(a.op, AssignOp::Assign);
        assert_eq!(a.line, 1);
    }

    #[test]
    fn raw_pointer_with_literal_address_folds_to_address() {
        let accesses = extract("void f(void) { *(volatile uint32_t *)(0x40028000 + 0x08) = 0x64; }");
        assert_eq!(accesses.len(), 1);
        assert_eq!(accesses[0].tier, AccessTier::RawPointer);
        assert_eq!(accesses[0].target, Target::Address(0x40028008));
        assert_eq!(accesses[0].value, Some(0x64));
    }

    #[test]
    fn struct_field_access_resolves_peripheral_and_register() {
        let accesses = extract("void f(void) { pll_sys_hw->fbdiv_int = 100; }");
        assert_eq!(accesses.len(), 1);
        let a = &accesses[0];
        assert_eq!(a.tier, AccessTier::StructField);
        assert_eq!(
            a.target,
            Target::StructField {
                peripheral: "PLL_SYS".to_string(),
                register: "FBDIV_INT".to_string()
            }
        );
        assert_eq!(a.value, Some(100));
    }

    #[test]
    fn compound_assignment_op_is_captured() {
        let accesses = extract("void f(void) { pll_sys_hw->cs |= PLL_SYS_CS_BYPASS_BITS; }");
        assert_eq!(accesses.len(), 1);
        assert_eq!(accesses[0].op, AssignOp::OrAssign);
        // RHS is a macro identifier: not foldable, must not be guessed.
        assert_eq!(accesses[0].value, None);
        assert_eq!(accesses[0].raw_rhs, "PLL_SYS_CS_BYPASS_BITS");
    }

    #[test]
    fn pointer_deref_of_plain_variable_is_unresolved_not_dropped() {
        let accesses = extract("void f(volatile uint32_t *reg) { *reg = 5; }");
        assert_eq!(accesses.len(), 1);
        assert_eq!(accesses[0].tier, AccessTier::RawPointer);
        assert_eq!(accesses[0].target, Target::Unresolved);
    }

    #[test]
    fn struct_access_on_non_hw_variable_is_unresolved() {
        let accesses = extract("void f(void) { my_state->counter = 5; }");
        assert_eq!(accesses.len(), 1);
        assert_eq!(accesses[0].target, Target::Unresolved);
    }

    #[test]
    fn plain_local_variable_assignment_is_not_a_register_access() {
        let accesses = extract("void f(void) { int x = 5; x = 6; }");
        assert!(accesses.is_empty());
    }

    #[test]
    fn dot_field_access_is_not_treated_as_struct_tier() {
        // `.` is a value-type member, not a hardware-struct pointer access.
        let accesses = extract("void f(void) { thing.member = 5; }");
        assert!(accesses.is_empty());
    }

    #[test]
    fn shifted_mask_value_is_folded() {
        let accesses = extract("void f(void) { pll_sys_hw->cs = (1u << 8) | (1u << 5); }");
        assert_eq!(accesses[0].value, Some((1u64 << 8) | (1u64 << 5)));
    }
}
