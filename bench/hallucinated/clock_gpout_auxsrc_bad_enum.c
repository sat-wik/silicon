#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct clocks_hw_t {
    io_rw_32 clk_gpout0_ctrl;
};

#define clocks_hw ((struct clocks_hw_t *)0x40008000u)

// BUG: AUXSRC (bits [8:5]) only has enumerated values 0-10 in the RP2040
// SVD; 12 is not one of them (a hallucinated/invalid clock source select).
void set_gpout0_auxsrc_to_invalid_source(void) {
    clocks_hw->clk_gpout0_ctrl = 12u << 5;
}
