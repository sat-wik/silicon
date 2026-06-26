#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct clocks_hw_t {
    io_rw_32 clk_gpout0_ctrl;
};

#define clocks_hw ((struct clocks_hw_t *)0x40008000u)

// AUXSRC occupies bits [8:5]; clksrc_pll_usb = 3 is a valid enum value.
void set_gpout0_auxsrc_to_pll_usb(void) {
    clocks_hw->clk_gpout0_ctrl = 3u << 5;
}
