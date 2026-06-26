# Hardware demo

Proves a `silicon` finding maps to real silicon behavior, not just a static
nitpick: `blink_hallucinated.c` differs from `blink_correct.c` by exactly one
register value, `silicon` flags that line statically, and the predicted
consequence — the onboard LED never blinks — is observable on a real board.

## What's planted

Both files blink GPIO25 (the LED on a standard Pico) via direct register
writes: route the pin to the SIO peripheral by setting
`IO_BANK0.GPIO25_CTRL.FUNCSEL`, enable it as an output in `SIO.GPIO_OE`, then
toggle `SIO.GPIO_OUT` in a loop.

- `blink_correct.c` sets `FUNCSEL = 5` (the SVD's `sio_25` enum value).
- `blink_hallucinated.c` sets `FUNCSEL = 0` — not one of GPIO25's enumerated
  function-select values (valid: 1,2,3,4,5,6,7,8,9,31). With FUNCSEL wrong,
  GPIO25 is never actually routed to SIO, so the GPIO_OUT/GPIO_OE writes
  later in the file have no effect on the physical pin: **the LED stays off,
  even though the firmware "looks" like it's blinking it.**

## Requirements

- A standard Raspberry Pi **Pico** (not Pico W — its LED is wired through the
  CYW43 wireless chip over SPI, not a plain RP2040 GPIO, so this demo won't
  show anything on a Pico W).
- A micro-USB cable.
- The [Raspberry Pi Pico SDK](https://github.com/raspberrypi/pico-sdk) and an
  `arm-none-eabi` toolchain, per its own setup instructions.
- CMake.

## Steps

1. **Static finding first, no hardware needed:**
   ```
   cargo run -p silicon-cli -- demo/blink_hallucinated.c
   ```
   This should cite `IO_BANK0.GPIO25_CTRL.FUNCSEL` and the invalid value 0.

2. **Set up the build** (one-time):
   ```
   export PICO_SDK_PATH=/path/to/pico-sdk
   cp "$PICO_SDK_PATH/external/pico_sdk_import.cmake" demo/
   cd demo && mkdir build && cd build && cmake .. && make -j
   ```

3. **Flash the correct version**: hold BOOTSEL while plugging the Pico into
   USB (it mounts as a USB drive), then drag `build/blink_correct.uf2` onto
   it. The board reboots and runs it immediately.
   **Expected:** the onboard LED blinks at ~1 Hz.

4. **Flash the hallucinated version**: hold BOOTSEL and replug, then drag
   `build/blink_hallucinated.uf2` onto the drive.
   **Predicted:** the LED does not blink — stays off.

5. Compare what you observed against the prediction. If it matches, the
   static finding from step 1 has been confirmed on real silicon.
