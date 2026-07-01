#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct resets_hw_t {
    io_rw_32 reset;
    io_rw_32 wdsel;
    io_rw_32 reset_done;
};

#define resets_hw ((struct resets_hw_t *)0x4000c000u)

// Deassert reset for UART0 (bit 22) and ADC (bit 0) so those peripherals
// can be used. Clears bits via &= rather than using the atomic CLR alias
// to keep this in the raw-register tier silicon checks.
void deassert_uart0_and_adc(void) {
    resets_hw->reset &= ~((1u << 22) | (1u << 0));
}
