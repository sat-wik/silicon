// Sample RP2040 firmware exercising all three register/structs-tier
// access patterns in scope for M2 extraction. Mirrors the shapes real
// pico-sdk firmware uses for PLL setup, GPIO output, and clock muxing.

#include <stdint.h>

typedef volatile uint32_t io_rw_32;

#define PLL_SYS_BASE 0x40028000u
#define PLL_SYS_CS_OFFSET 0x00000000u
#define PLL_SYS_PWR_OFFSET 0x00000004u
#define PLL_SYS_FBDIV_INT_OFFSET 0x00000008u
#define PLL_SYS_CS_BYPASS_BITS 0x00000100u
#define PLL_SYS_PWR_VCOPD_BITS 0x00000020u

struct pll_hw {
    io_rw_32 cs;
    io_rw_32 pwr;
    io_rw_32 fbdiv_int;
    io_rw_32 prim;
};

#define pll_sys_hw ((struct pll_hw *)PLL_SYS_BASE)

struct sio_hw {
    io_rw_32 gpio_oe;
    io_rw_32 gpio_out;
};

#define sio_hw ((struct sio_hw *)0xd0000000u)

struct clocks_hw {
    io_rw_32 clk_gpout0_ctrl;
};

#define clocks_hw ((struct clocks_hw *)0x40008000u)

void pll_init(void) {
    // regs-constant tier: BASE + OFFSET macros
    *(io_rw_32 *)(PLL_SYS_BASE + PLL_SYS_PWR_OFFSET) |= PLL_SYS_PWR_VCOPD_BITS;
    *(io_rw_32 *)(PLL_SYS_BASE + PLL_SYS_FBDIV_INT_OFFSET) = 100;

    // structs tier
    pll_sys_hw->cs |= PLL_SYS_CS_BYPASS_BITS;
    pll_sys_hw->fbdiv_int = 100;

    // raw-pointer tier: foldable literal address
    *(io_rw_32 *)(0x40028000u + 0x00000000u) = 0x00000001u;
}

void gpio_init(uint32_t pin_mask) {
    // structs tier, compound assignment
    sio_hw->gpio_oe |= pin_mask;
    sio_hw->gpio_out = 0;
}

void clock_mux_select(uint32_t auxsrc) {
    // structs tier with a runtime (non-foldable) value: tier/shape known,
    // target known, value not statically determinable.
    clocks_hw->clk_gpout0_ctrl = auxsrc << 5;
}

void unresolved_examples(volatile uint32_t *reg, struct { io_rw_32 x; } *opaque) {
    // raw-pointer tier, but the address is a runtime variable: unresolved.
    *reg = 0xdeadbeefu;
    // structs tier, but the pointer doesn't follow the `_hw` naming convention: unresolved.
    opaque->x = 1;
}
