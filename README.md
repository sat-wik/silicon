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
- M2 (register-access extraction), M3 (checker), M4 (reports + CI), M5
  (benchmark + hardware demo) — not yet started.

## Layout

```
crates/svd-model/   M1: SVD → queryable model
crates/fw-parse/    M2: firmware → register-access list (not yet implemented)
crates/checker/     M3: model + accesses → findings (not yet implemented)
crates/report/      M4: terminal + SARIF emitters (not yet implemented)
cli/                CLI entrypoint
data/rp2040.svd     vendored from raspberrypi/pico-sdk (BSD-3-Clause, see data/PICO_SDK_LICENSE.TXT)
```

## Build & test

```
cargo build --workspace
cargo test -p svd-model
```
