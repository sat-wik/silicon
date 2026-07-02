# Silicon

**A static analysis tool that catches invalid register values in RP2040 firmware before you flash it to the chip.**

---

## The problem in one sentence

AI coding tools (and humans) write firmware that configures hardware using magic numbers — and sometimes those numbers are simply wrong for the chip. Silicon catches these mistakes automatically, before the firmware ever touches real hardware.

---

## What's an RP2040 and why does this matter?

The **RP2040** is the microcontroller chip inside the Raspberry Pi Pico (~$4). Like all microcontrollers, it has hundreds of **registers** — tiny memory locations that control every aspect of the hardware: which pin does what, how fast a clock runs, whether a peripheral is enabled, and so on.

Each register has **fields** (groups of bits), and each field only accepts specific values. For example:

- A 3-bit field might only accept values 0–7
- A GPIO function-select field might only accept values 0, 1, 2, 5, or 31 — not 99
- A read-only field must never be written to

These rules live in the chip's official **SVD file** (think: a machine-readable version of the 650-page datasheet). When AI tools generate firmware, they frequently hallucinate register names, field offsets, or values that don't match what the chip actually accepts. The firmware compiles fine and flashes fine — but the hardware silently misbehaves.

**Silicon reads your firmware and the official SVD, and reports every mismatch.**

---

## A real example

The `demo/` folder contains a firmware with one planted bug:

```c
// demo/blink_hallucinated.c
// Configure GPIO25 (the Pico's onboard LED) — WRONG value
*(volatile uint32_t *)(IO_BANK0_BASE + GPIO25_CTRL_OFFSET) = 0u;
//                                                            ^^^
//                                           FUNCSEL=0 is not a valid SIO
//                                           function selector — the LED will
//                                           never turn on
```

Silicon catches it instantly:

```
 ✗ error  field-value-not-in-enum
 GPIO25_CTRL.FUNCSEL — value 0 not in SVD enum

  ╭─ demo/blink_hallucinated.c · line 18
  │
  │  18  │  *(volatile uint32_t *)(IO_BANK0_BASE + GPIO25_CTRL_OFFSET) = 0u;
  │       │  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  │
  ╰─ SVD: IO_BANK0.GPIO25_CTRL.FUNCSEL
         allowed: xip=0, spi0_rx=1, uart0_tx=2, i2c0_sda=3, pwm_a=4,
                  sio=5, pio0=6, pio1=7, usb_muxing=9, null=31
```

This was flashed onto a real Pico and confirmed: the LED stays off. Fix the value to `5` (SIO) and it blinks correctly.

---

## What Silicon checks

**Register-level checks** (when writing directly to memory-mapped addresses):

| What it catches | Example |
|---|---|
| Register name doesn't exist for this peripheral | `sio_hw->not_a_real_register = 1` |
| Address inside a peripheral but not at a register boundary | `*(uint32_t*)(SIO_BASE + 1) = 1` |
| Value doesn't fit in the register's bit width | Writing 5000 to a 12-bit register |
| Value sets bits outside all defined fields (reserved bits) | Writing to bits the SVD doesn't define |
| Value not in the chip's allowed list for that field | `FUNCSEL = 99` when only 0–9, 31 are valid |
| Writing to a read-only register | `cpuid_hw->value = 5` |
| Writing to a read-only field | Setting reserved bits |

**HAL call checks** (when using the Pico SDK's helper functions):

| Function | What's checked |
|---|---|
| `gpio_init`, `gpio_put`, `gpio_set_dir` | Pin number must be 0–29 |
| `gpio_set_function` | Function selector must be in the SVD enum for that pin |
| `uart_init`, `spi_init`, `i2c_init` | Instance must be `uart0`/`uart1`, `spi0`/`spi1`, `i2c0`/`i2c1` |
| `pwm_set_wrap`, `pwm_set_chan_level` | Slice 0–7; channel 0 or 1 |
| `dma_channel_configure` | Channel must be 0–11 |
| `adc_select_input` | Channel must fit in ADC CS.AINSEL's bit width |

**Every finding is grounded in the SVD — never guessed.** If Silicon can't verify something (no enum in the SVD, value not statically known), it emits an informational note instead of a false positive.

---

## Installation

You need [Rust](https://rustup.rs) installed. Then:

```bash
git clone https://github.com/sat-wik/silicon
cd silicon
cargo install --path cli
```

That's it. The RP2040 SVD is baked into the binary — no separate data files needed.

---

## Basic usage

```bash
# Check a single file
silicon main.c

# Check a whole directory (scans all *.c files recursively)
silicon src/

# Also scan header files
silicon --scan-headers src/

# Write the report to a file
silicon --format sarif --output results.sarif src/
```

The exit code is `0` if no errors were found, `1` if any errors exceeded the threshold. This makes it drop-in for CI.

---

## All commands

### `silicon check` — the default

```bash
silicon [paths...]                  # check firmware files or directories
silicon --fail-on warning [paths]   # fail on warnings too
silicon --fail-on none   [paths]    # always exit 0 (for reporting only)
silicon --notes          [paths]    # also show informational notes
silicon --scan-headers   [paths]    # also scan *.h files
silicon --format sarif   [paths]    # output SARIF instead of text
silicon --output report.sarif [paths]  # write to file instead of stdout
silicon --watch          [paths]    # re-run on every file change (Ctrl-C to stop)
```

### `silicon explain` — understand a rule

Don't know what a finding means? Ask Silicon to explain it:

```bash
silicon explain field-value-not-in-enum
silicon explain hal-call-instance-not-in-svd
silicon explain write-to-read-only-register
```

Output includes what the rule checks, the SVD fact behind it, and a before/after C example.

Available rule IDs: `nonexistent-register`, `address-not-a-register`, `value-exceeds-register-width`, `value-sets-undefined-bits`, `field-value-not-in-enum`, `write-to-read-only-register`, `write-to-read-only-field`, `hal-call-pin-out-of-range`, `hal-call-funcsel-not-in-enum`, `hal-call-index-out-of-range`, `hal-call-instance-not-in-svd`.

### `silicon list-*` — explore the SVD

Browse what the chip actually defines without opening the 650-page datasheet:

```bash
# What peripherals exist?
silicon list-peripherals

# What registers does the CLOCKS peripheral have?
silicon list-registers CLOCKS

# What fields does CLK_GPOUT0_CTRL have, and what values are allowed?
silicon list-fields CLOCKS CLK_GPOUT0_CTRL
```

Example output of `silicon list-fields CLOCKS CLK_GPOUT0_CTRL`:

```
 CLOCKS.CLK_GPOUT0_CTRL  (offset 0x000, 32-bit, readwrite)

 FIELD       BITS      ACCESS    ALLOWED VALUES
 ────────────────────────────────────────────────────────────
 AUXSRC      [8:5]     rw        clksrc_pll_sys=0, clksrc_gpin0=1 … (11 values)
 KILL        [10]      rw        (no SVD enum)
 ENABLE      [11]      rw        (no SVD enum)
 PHASE       [17:16]   rw        (no SVD enum)
 NUDGE       [20]      rw        (no SVD enum)
```

### `silicon install-hook` — block bad commits

Install a git pre-commit hook so Silicon runs automatically before every `git commit`:

```bash
silicon install-hook
```

This creates `.git/hooks/pre-commit`. If a commit would introduce an error-severity finding, the commit is blocked and the finding is shown. Remove the file to disable.

### Baseline suppression — gradual adoption

Already have a codebase with existing issues? Write a baseline so Silicon only alerts you to *new* problems:

```bash
# Snapshot today's findings
silicon --write-baseline silicon.baseline src/

# From now on, only new findings (regressions) will be reported
silicon --baseline silicon.baseline src/
```

---

## Config file (`.silicon.toml`)

Create `.silicon.toml` in your project root so you don't have to pass flags every time:

```toml
# .silicon.toml
paths = ["src", "include"]
scan-headers = true
fail-on = "error"
```

CLI flags always override the config file. Copy `.silicon.toml.example` from this repo to get started.

---

## VS Code extension

The `vscode/` directory contains a TypeScript extension that runs Silicon automatically when you open or save a `.c` or `.cpp` file and shows findings as inline squiggles — red for errors, yellow for warnings.

**To install for development:**

```bash
cd vscode
npm install
npm run compile
# Then press F5 in VS Code to launch an Extension Development Host
```

**Settings (in VS Code `settings.json`):**

```json
{
  "silicon.executablePath": "silicon",   // path to the binary
  "silicon.runOnSave": true,             // re-check on every save
  "silicon.extraArgs": ["--scan-headers"]
}
```

---

## GitHub Action

Add Silicon as a CI check on every pull request. Create `.github/workflows/silicon.yml`:

```yaml
name: Silicon register check
on: [push, pull_request]

jobs:
  silicon:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: sat-wik/silicon/action@main
        with:
          paths: src/
          fail-on: error
```

Findings show up as annotations on the diff. If you use GitHub's code-scanning UI, pass `sarif-upload: true` to get persistent finding tracking.

---

## How it works (for the technically curious)

Silicon has four stages, each in its own Rust crate:

```
1. svd-model   Parse the RP2040's CMSIS-SVD file into a queryable model:
               peripheral → register → field → bit width / access / allowed values

2. fw-parse    Walk the firmware's C source with tree-sitter (a real parser,
               not regex) and extract every register write: what was written,
               to what address/field, with what value. Constant-fold #define
               macros across all files. Mark anything not statically
               determinable as "unresolved" — never guess.

3. checker     For each resolved write, look it up in the SVD model and check
               the six violation classes. Conservative: anything uncertain
               becomes an informational note, not a finding.

4. report      Render findings as a terminal report with ANSI color and source
               excerpts, or as SARIF 2.1.0 for code-scanning UIs.
```

**The core invariant:** every finding is grounded 100% in a parsed SVD fact. The SVD is vendored from the official [raspberrypi/pico-sdk](https://github.com/raspberrypi/pico-sdk) (BSD-3-Clause, see `data/PICO_SDK_LICENSE.TXT`).

---

## Project layout

```
crates/svd-model/    Stage 1: SVD → queryable model
crates/fw-parse/     Stage 2: C firmware → register access list
crates/checker/      Stage 3: accesses vs model → findings
crates/report/       Stage 4: terminal + SARIF output
cli/                 The silicon binary (all four stages wired together)
action/              GitHub Action wrapper
vscode/              VS Code extension (TypeScript)
data/rp2040.svd      Vendored RP2040 SVD (BSD-3-Clause)
bench/correct/       Known-correct RP2040 firmware samples
bench/hallucinated/  Firmware with planted register bugs (labeled)
demo/                Hardware demo: planted bug confirmed on a real Pico
```

## Build and test

```bash
cargo build --workspace
cargo test --workspace
```

All tests are deterministic and require no hardware — they run against the vendored SVD.
