# Silicon

RP2040 firmware register-correctness verifier. Checks register/bitfield usage
in C/C++ firmware against the chip's CMSIS-SVD ground truth, catching
nonexistent registers/fields, out-of-range or non-enumerated values, and
writes to read-only/reserved fields before flash. See [CLAUDE.md](CLAUDE.md)
for the full project spec and milestone plan.

## Status

- **M1 (SVD ground-truth model) — done.** `crates/svd-model` parses the
  vendored `data/rp2040.svd` into a queryable peripheral → register → field
  model and answers bit width / access / allowed values / reset value for
  any field, explicitly reporting "no enum in SVD" rather than guessing.
- **M2 (register-access extraction) — done.** `crates/fw-parse` walks firmware
  with `tree-sitter-c` and extracts the three in-scope access shapes (raw
  pointer dereference writes, `hardware_structs` `->` field writes, and
  `hardware/regs` `_BASE`/`_OFFSET` macro arithmetic), best-effort naming the
  peripheral/register by source convention and constant-folding the written
  value where possible. Anything not statically determinable is tagged
  `Target::Unresolved`, never guessed.
- **M3 (the checker) — done.** `crates/checker` resolves each access against
  the SVD model and flags the FR6 violation classes: nonexistent register,
  wrong address/offset, value too wide / setting undefined bits, value not
  in the SVD enum, and writes to read-only fields/registers. Anything not
  statically resolvable, or unverifiable (no SVD enum), is an informational
  `Note`, never a `Finding` — only SVD-grounded violations gate.
- **M4 (evidence report + CI) — done.** `crates/report` renders findings as
  a terminal report (file/line, offending expression, severity, plain-
  language explanation citing the SVD) or a SARIF document. The `silicon`
  CLI (vendored RP2040 SVD baked in, no config needed) exits non-zero above
  a configurable `--fail-on` severity, and `action/` is a composite GitHub
  Action wrapping it with optional SARIF upload for code scanning.
- M5 (benchmark + hardware demo) — not yet started.

## Layout

```
crates/svd-model/   M1: SVD → queryable model
crates/fw-parse/    M2: firmware → register-access list
crates/checker/     M3: model + accesses → findings
crates/report/      M4: terminal + SARIF emitters
cli/                M4: CLI entrypoint (silicon)
action/             M4: GitHub Action wrapper
.github/workflows/  CI for this repo
data/rp2040.svd     vendored from raspberrypi/pico-sdk (BSD-3-Clause, see data/PICO_SDK_LICENSE.TXT)
bench/correct/      known-correct RP2040 firmware samples
bench/hallucinated/ firmware with a single planted register/bitfield bug, paired 1:1 with bench/correct/
```

## Build & test

```
cargo build --workspace
cargo test --workspace
```

## CLI usage

```
silicon [--svd <path>] [--format text|sarif] [--fail-on error|warning|none] [--output <path>] [--notes] <paths...>
```

`<paths...>` are firmware files or directories (scanned recursively for
`*.c`). With no `--svd`, the vendored RP2040 SVD baked into the binary is
used. Exit code is non-zero iff a finding's severity meets or exceeds
`--fail-on` (default `error`).
