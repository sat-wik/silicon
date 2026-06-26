# CLAUDE.md — Silicon (RP2040 Register-Correctness Verifier)

This file orients Claude Code for building **Silicon**, a static analysis tool that verifies RP2040 firmware register usage against the chip's SVD ground truth, catching AI-hallucinated / invalid register and bitfield values before flash. Read this fully before writing code. See `PRD.md` for the product rationale.

---

## What this project is (one paragraph)

Silicon parses RP2040 firmware C/C++, finds every register access at the **register/structs tier** (raw memory-mapped writes, `hardware_structs` field access, `hardware/regs` constants), resolves each against a structured model of the RP2040 register map (built from the SDK-shipped CMSIS-SVD), and reports any access that is invalid for the device — nonexistent register/field, out-of-range or non-enumerated value, value too wide for the field, write to a read-only/reserved field, or wrong address/offset. Output is a deterministic, evidence-grounded report (terminal + SARIF) suitable as a CI gate.

## Core invariants (do not violate)

1. **Evidence comes from the SVD, never from an LLM.** The checker's findings must be 100% deterministic and traceable to a parsed SVD fact. An optional LLM layer may *rephrase* a finding into friendlier prose, but must never decide whether something is a violation. If you find yourself asking a model "is this register write valid?", stop — that answer must come from the SVD model.
2. **Precision over recall.** A false positive on correct code is worse than a missed bug, because false positives get the tool disabled. When unsure whether something is a violation, do NOT flag it. Every check must be conservative.
3. **v1 scope is the register/structs tier only.** IN: raw `*(uint32_t*)(BASE+OFF) = v`, `peripheral_hw->field = v` struct access, `hardware/regs` macro constants. OUT (defer to Phase B): high-level `pico_*`/`gpio_*` SDK calls. Do not attempt to model HAL-call semantics in v1.
4. **Deterministic core.** Same firmware + same SVD → byte-identical report. No randomness, no network calls in the checker path.
5. **Real parser, not regex.** Use a proper C parser (tree-sitter-c first for simplicity, libclang if precision demands it). Never resolve register accesses with regex — macros and pointer math will defeat it and create false positives.
6. **Handle SVD imperfection explicitly.** RP2040 SVD may lack enumerations for some fields. If a field has no enumerated values in the SVD, you CANNOT check value-membership for it — record "unverifiable (no enum in SVD)" rather than guessing. Bounded recall is acceptable and must be stated; false positives from guessing are not.

## Repository structure (target)

```
silicon/
├── CLAUDE.md                 # this file
├── PRD.md                    # product rationale
├── README.md                 # usage, install, demo
├── crates/ (or pkg/)         # language TBD — see "Stack decision" below
│   ├── svd-model/            # M1: parse SVD → queryable register/field model
│   ├── fw-parse/             # M2: parse firmware → register-access list
│   ├── checker/              # M3: resolve accesses vs model → findings
│   └── report/               # M4: terminal + SARIF emitters
├── cli/                      # CLI entrypoint + exit-code gating
├── action/                   # M4: GitHub Action wrapper
├── data/
│   └── rp2040.svd            # SDK-shipped RP2040 SVD (vendored, with license)
├── bench/                    # M5: labeled correct/buggy firmware corpus
│   ├── correct/
│   └── hallucinated/         # firmware with known register bugs + labels
└── demo/                     # M5: buggy firmware + board instructions
```

## Stack decision (resolve before M2)

- **Checker language:** Rust preferred (single static binary, fast, strong for a long-lived deterministic tool; mirrors the transparent-analysis pattern Satwik used in Tollgate). Go acceptable if velocity matters more. Decide before M2 and don't churn it.
- **SVD parsing (M1):** do NOT hand-roll. Reuse an existing CMSIS-SVD parser (e.g., the Rust `svd-parser` crate, or the Python `cmsis-svd` lib) to build the model. Building the parser is not the contribution; the checker is.
- **Firmware parsing (M2):** start with `tree-sitter-c` (easy, good enough to find register-access patterns). Escalate to `libclang` only if macro expansion / type resolution proves necessary for precision.
- **RP2040 SVD source (M1):** vendored from the Pico SDK. Include the vendor license in `data/`.

## Build milestones (each ends with a working, demoable artifact)

### M1 — SVD ground-truth model
- Parse `data/rp2040.svd` into a queryable model: peripherals → registers (name, address, offset, size, access) → fields (name, bit offset/width, access, enumerated values if present, reset value).
- Provide a query API: "for peripheral P register R field F, what are: bit width, access, allowed values (or none), reset value?"
- **Done when:** a unit test answers "is value V valid for `SIO.GPIO_OE` / `PLL_SYS.FBDIV_INT`?" correctly, including the "no enum → unverifiable" case.

### M2 — Register-access extraction
- Parse firmware; emit a list of register accesses, each tagged with tier (raw-pointer / structs / regs-constant), resolved target (peripheral/register/field where determinable), the written value (where statically known), file/line.
- Must handle: `*(volatile uint32_t*)(BASE+OFF) = v`, `peripheral_hw->field = v`, `reg |= MASK`, and `hardware/regs` macro constants. Mark accesses whose target or value is NOT statically determinable as "unresolved" (do not guess).
- **Done when:** running on a sample RP2040 file lists every register access with resolved meaning, and clearly marks the unresolved ones.

### M3 — The checker (the core)
- For each *resolved* access, look up the model and flag the FR6 violation classes: nonexistent register/field; value out of range / not in enum (only when enum exists); value too wide for field width; write to RO/reserved; wrong offset/address.
- Conservative: unresolved accesses and no-enum fields produce informational notes, NOT violations.
- **Done when:** on a firmware file with a planted invalid bitfield (e.g., a too-wide value or a value not in the SVD enum), the checker flags exactly that line with the SVD citation, and produces ZERO findings on the known-correct equivalent.

### M4 — Evidence report + CI
- Terminal report: file/line, offending expression, SVD citation (peripheral/register/field, allowed values, access), severity, plain-language explanation.
- SARIF output for code-scanning UIs.
- CLI exits non-zero above a configurable severity; GitHub Action wrapper.
- **Done when:** a demo PR fails CI with a cited register finding and passes once fixed.

### M5 — Hardware demo + precision/recall measurement
- `bench/`: a labeled corpus — known-correct RP2040 firmware + firmware with known register bugs (generate via several AI coding tools, hand-label the planted/real bugs). Measure precision (headline) and recall.
- `demo/`: a buggy firmware whose flagged register error produces a *visible, predicted* misbehavior on a real Pico (~$5), proving the static finding maps to real silicon behavior.
- **Done when:** precision/recall numbers exist on the labeled set, and the board reproduces the predicted misbehavior the tool flagged.

## Definition of done (v1)
- Runs as a CLI + GitHub Action on RP2040 firmware.
- Catches the FR6 register-violation classes at the register/structs tier with high precision.
- Every finding cites SVD ground truth; core is fully deterministic.
- Labeled-benchmark precision/recall reported honestly, including the "unverifiable (no SVD enum)" recall ceiling.
- Hardware demo reproduces a flagged bug on a real board.

## Explicit non-goals for v1 (do not build these)
- High-level `pico_*`/`gpio_*` HAL-call semantics/sequence checking (→ Phase B).
- Concurrency/ISR/DMA hazard detection (→ future work).
- Firmware generation or auto-fixing.
- Multi-chip support beyond RP2040.
- Any check whose ground truth is an LLM opinion rather than the SVD/datasheet.

## Honest framing (for README / interview)
The SVD ground truth and SVD parsers are standard and reused deliberately — the contribution is the **checker**: evidence-grounded static verification of firmware register usage that catches AI-hallucinated/invalid register values pre-flash, scoped to the RP2040 register/structs tier where ground truth is exact. Do not claim novelty for SVD parsing or for "AI firmware verification" as a category; claim the specific, working, precise checker.

## Open decisions to surface to Satwik (don't silently choose)
- Rust vs Go for the checker core.
- tree-sitter-c vs libclang for firmware parsing (start tree-sitter; escalate only if needed).
- Whether the optional LLM explanation layer ships in v1 or is held back to keep the "fully deterministic" story clean (default: hold back for v1).
- How the labeled benchmark is constructed and validated (which AI tools generate the buggy corpus; how bugs are confirmed real).
