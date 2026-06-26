// Identical to blink_correct.c except for one line: FUNCSEL is set to 0
// instead of 5. 0 is not one of IO_BANK0.GPIO25_CTRL.FUNCSEL's enumerated
// values for this pin (valid: 1,2,3,4,5,6,7,8,9,31) — the kind of plausible-
// looking but invalid register value an AI assistant can hallucinate.
//
// Run `silicon demo/blink_hallucinated.c` before flashing this: it cites
// the violation statically, with zero hardware involved. Flashing it then
// demonstrates that the static finding maps to a real, predicted failure:
// the LED never blinks, because GPIO25 is never actually routed to SIO, so
// the GPIO_OUT/GPIO_OE writes below have no effect on the physical pin.

#include "pico/stdlib.h"
#include <stdint.h>

#define IO_BANK0_BASE 0x40014000u
#define GPIO25_CTRL_OFFSET 0x000000ccu

#define SIO_BASE 0xd0000000u
#define SIO_GPIO_OE_OFFSET 0x00000020u
#define SIO_GPIO_OUT_OFFSET 0x00000010u

#define GPIO25_BIT (1u << 25)

int main(void) {
    *(volatile uint32_t *)(IO_BANK0_BASE + GPIO25_CTRL_OFFSET) = 0u; // BUG: invalid FUNCSEL
    *(volatile uint32_t *)(SIO_BASE + SIO_GPIO_OE_OFFSET) |= GPIO25_BIT;

    while (1) {
        *(volatile uint32_t *)(SIO_BASE + SIO_GPIO_OUT_OFFSET) |= GPIO25_BIT;
        sleep_ms(500);
        *(volatile uint32_t *)(SIO_BASE + SIO_GPIO_OUT_OFFSET) &= ~GPIO25_BIT;
        sleep_ms(500);
    }
}
