//! M2: parses RP2040 firmware C/C++ into a list of register accesses.
//!
//! This crate only extracts and best-effort *names* what an assignment
//! touches (by source-level naming convention: `<peripheral>_hw->member`,
//! `<PERIPHERAL>_BASE + <PERIPHERAL>_<REGISTER>_OFFSET`, or a constant-folded
//! literal address). It never decides whether that target is valid — it has
//! no SVD model. Anything not statically determinable is tagged
//! `Target::Unresolved` rather than guessed; resolving against ground truth
//! is the checker crate's job (M3), per CLAUDE.md invariant 1.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::Node;

type MacroTable = HashMap<String, u64>;

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

/// A recognized Pico SDK (Phase B) function call with statically extracted
/// arguments. Only calls to the explicitly-modeled SDK functions are emitted.
#[derive(Debug, Clone)]
pub struct HalCall {
    pub function: String,
    /// Each argument, constant-folded if possible. `None` means the value
    /// isn't statically determinable — checker will note, not guess.
    pub args: Vec<Option<u64>>,
    pub raw_args: Vec<String>,
    pub file: PathBuf,
    pub line: usize,
}

/// SDK function names the Phase B HAL-call tier recognises. Anything else is
/// simply not extracted — not a finding, not a note, just out of scope.
const HAL_FUNCTIONS: &[&str] = &["gpio_set_function", "gpio_init", "gpio_put", "gpio_set_dir"];

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

    let bytes = source.as_bytes();
    let macros = build_macro_table(tree.root_node(), bytes);
    let mut out = Vec::new();
    walk(tree.root_node(), bytes, file, &macros, &mut out);
    out
}

/// Walks `source` for calls to recognised Pico SDK functions and extracts
/// their arguments (constant-folded where possible).
pub fn extract_hal_calls(source: &str, file: &Path) -> Vec<HalCall> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("tree-sitter-c grammar must load");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let bytes = source.as_bytes();
    let macros = build_macro_table(tree.root_node(), bytes);
    let mut out = Vec::new();
    walk_hal(tree.root_node(), bytes, file, &macros, &mut out);
    out
}

/// Collects every `#define NAME integer_literal` in the translation unit into
/// a lookup table used by `fold_const`. Only simple numeric-literal values are
/// stored — complex expressions like `(1u << 25)` remain unresolved rather
/// than guessed, consistent with CLAUDE.md invariant 2 (precision over recall).
fn build_macro_table(root: Node, src: &[u8]) -> MacroTable {
    let mut table = MacroTable::new();
    collect_macros(root, src, &mut table);
    table
}

fn collect_macros(node: Node, src: &[u8], table: &mut MacroTable) {
    if node.kind() == "preproc_def" {
        if let (Some(name_node), Some(val_node)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        ) {
            let val_text = text(val_node, src).trim();
            if let Some(v) = parse_number_literal(val_text) {
                table.insert(text(name_node, src).to_string(), v);
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_macros(child, src, table);
    }
}

fn walk_hal(node: Node, src: &[u8], file: &Path, macros: &MacroTable, out: &mut Vec<HalCall>) {
    if node.kind() == "call_expression" {
        if let Some(call) = extract_hal_call(node, src, file, macros) {
            out.push(call);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_hal(child, src, file, macros, out);
    }
}

fn extract_hal_call(node: Node, src: &[u8], file: &Path, macros: &MacroTable) -> Option<HalCall> {
    let fn_node = node.child_by_field_name("function")?;
    if fn_node.kind() != "identifier" {
        return None;
    }
    let function = text(fn_node, src);
    if !HAL_FUNCTIONS.contains(&function) {
        return None;
    }
    let args_node = node.child_by_field_name("arguments")?;
    let mut cursor = args_node.walk();
    let arg_nodes: Vec<Node> = args_node.named_children(&mut cursor).collect();
    let args: Vec<Option<u64>> = arg_nodes.iter().map(|a| fold_const(*a, src, macros)).collect();
    let raw_args: Vec<String> = arg_nodes.iter().map(|a| text(*a, src).to_string()).collect();
    Some(HalCall {
        function: function.to_string(),
        args,
        raw_args,
        file: file.to_path_buf(),
        line: node.start_position().row + 1,
    })
}

fn walk(node: Node, src: &[u8], file: &Path, macros: &MacroTable, out: &mut Vec<RegisterAccess>) {
    if node.kind() == "assignment_expression" {
        if let Some(access) = resolve_assignment(node, src, file, macros) {
            out.push(access);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, file, macros, out);
    }
}

fn resolve_assignment(node: Node, src: &[u8], file: &Path, macros: &MacroTable) -> Option<RegisterAccess> {
    let op_node = node.child_by_field_name("operator")?;
    let op = AssignOp::from_str(text(op_node, src))?;
    let left = node.child_by_field_name("left")?;
    let right = node.child_by_field_name("right")?;

    let (tier, target) = resolve_lhs(left, src, macros)?;
    let value = fold_const(right, src, macros);
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
fn resolve_lhs<'a>(left: Node<'a>, src: &'a [u8], macros: &MacroTable) -> Option<(AccessTier, Target)> {
    match left.kind() {
        "pointer_expression" => {
            if text(left.child_by_field_name("operator")?, src) != "*" {
                return None;
            }
            let argument = left.child_by_field_name("argument")?;
            let inner = unwrap_cast_and_parens(argument, src);
            Some(resolve_address_expr(inner, src, macros))
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

fn resolve_address_expr(node: Node, src: &[u8], macros: &MacroTable) -> (AccessTier, Target) {
    // Pure literal or macro-resolved arithmetic: a concrete address.
    if let Some(addr) = fold_const(node, src, macros) {
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

/// Constant-folds integer-literal arithmetic. Identifiers are resolved via
/// `macros` (populated from `#define NAME integer` in the same file). An
/// identifier absent from the table — or any non-constant subexpression —
/// makes the whole expression unfoldable; returns `None` rather than guessing.
fn fold_const(node: Node, src: &[u8], macros: &MacroTable) -> Option<u64> {
    match node.kind() {
        "number_literal" => parse_number_literal(text(node, src)),
        "identifier" => macros.get(text(node, src)).copied(),
        "parenthesized_expression" => fold_const(node.named_child(0)?, src, macros),
        "unary_expression" => {
            let op = text(node.child_by_field_name("operator")?, src);
            let v = fold_const(node.child_by_field_name("argument")?, src, macros)?;
            match op {
                "-" => Some(v.wrapping_neg()),
                "~" => Some(!v),
                "+" => Some(v),
                _ => None,
            }
        }
        "binary_expression" => {
            let op = text(node.child_by_field_name("operator")?, src);
            let l = fold_const(node.child_by_field_name("left")?, src, macros)?;
            let r = fold_const(node.child_by_field_name("right")?, src, macros)?;
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

    #[test]
    fn macro_defined_value_is_folded() {
        let src = "#define MY_FUNCSEL 5u\nvoid f(void) { pll_sys_hw->funcsel = MY_FUNCSEL; }";
        let accesses = extract(src);
        assert_eq!(accesses[0].value, Some(5));
    }

    #[test]
    fn macro_defined_address_folds_to_target_address() {
        let src = "#define MY_BASE 0x40028000u\n#define MY_OFF 0x08u\n\
                   void f(void) { *(volatile uint32_t *)(MY_BASE + MY_OFF) = 1u; }";
        let accesses = extract(src);
        assert_eq!(accesses[0].target, Target::Address(0x40028008));
        assert_eq!(accesses[0].value, Some(1));
    }

    #[test]
    fn macro_composed_with_literal_folds_correctly() {
        let src = "#define SHIFT 5u\nvoid f(void) { pll_sys_hw->cs = (1u << SHIFT); }";
        let accesses = extract(src);
        assert_eq!(accesses[0].value, Some(1u64 << 5));
    }

    #[test]
    fn undefined_macro_identifier_stays_unresolved() {
        // No #define in scope: value must stay None, not guessed.
        let accesses = extract("void f(void) { pll_sys_hw->cs |= SOME_UNKNOWN_BITS; }");
        assert_eq!(accesses[0].value, None);
    }
}
