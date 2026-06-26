// Blinks the onboard LED on a standard Raspberry Pi Pico (RP2040, LED on
// GPIO25) using direct register writes only — the register/structs tier
// `silicon` checks, not the Pico SDK's gpio_* helpers.
//
// To drive GPIO25 from the SIO peripheral's GPIO_OUT/GPIO_OE registers, the
// pin's function-select mux (IO_BANK0.GPIO25_CTRL.FUNCSEL) must first be set
// to 5 ("sio_25"). See blink_hallucinated.c for what happens when that
// value is wrong.

#include "pico/stdlib.h"
#include <stdint.h>

#define IO_BANK0_BASE 0x40014000u
#define GPIO25_CTRL_OFFSET 0x000000ccu

#define SIO_BASE 0xd0000000u
#define SIO_GPIO_OE_OFFSET 0x00000020u
#define SIO_GPIO_OUT_OFFSET 0x00000010u

#define GPIO25_BIT (1u << 25)

int main(void) {
    *(volatile uint32_t *)(IO_BANK0_BASE + GPIO25_CTRL_OFFSET) = 5u; // FUNCSEL: sio_25
    *(volatile uint32_t *)(SIO_BASE + SIO_GPIO_OE_OFFSET) |= GPIO25_BIT;

    while (1) {
        *(volatile uint32_t *)(SIO_BASE + SIO_GPIO_OUT_OFFSET) |= GPIO25_BIT;
        sleep_ms(500);
        *(volatile uint32_t *)(SIO_BASE + SIO_GPIO_OUT_OFFSET) &= ~GPIO25_BIT;
        sleep_ms(500);
    }
}
