#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct pll_hw_t {
    io_rw_32 cs;
    io_rw_32 pwr;
    io_rw_32 fbdiv_int;
};

#define pll_sys_hw ((struct pll_hw_t *)0x40028000u)

// BUG: FBDIV_INT is a 12-bit field (max 4095); 5000 overflows it and spills
// into bits the SVD defines as undefined for this register.
void set_fbdiv_too_wide(void) {
    pll_sys_hw->fbdiv_int = 5000;
}
