#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct pll_hw_t {
    io_rw_32 cs;
    io_rw_32 pwr;
    io_rw_32 fbdiv_int;
    io_rw_32 fbdiv_frac;
};

#define pll_usb_hw ((struct pll_hw_t *)0x4002c000u)

// Configure the USB PLL's feedback divisor for a 480MHz VCO from a 12MHz
// reference: 40 integer steps plus a fractional trim for a closer match.
void set_usb_pll_fbdiv(void) {
    pll_usb_hw->fbdiv_int = 40;
    pll_usb_hw->fbdiv_frac = 100;
}
