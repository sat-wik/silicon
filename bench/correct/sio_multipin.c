#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct sio_hw_t {
    io_rw_32 cpuid;
    io_rw_32 gpio_in;
    io_rw_32 gpio_hi_in;
    io_rw_32 _pad;
    io_rw_32 gpio_out;
    io_rw_32 gpio_out_set;
    io_rw_32 gpio_out_clr;
    io_rw_32 gpio_out_xor;
    io_rw_32 gpio_oe;
    io_rw_32 gpio_oe_set;
    io_rw_32 gpio_oe_clr;
    io_rw_32 gpio_oe_xor;
};

#define sio_hw ((struct sio_hw_t *)0xd0000000u)

// Configure GPIO 2, 3, and 4 as outputs and drive them high together.
// Uses the atomic SET aliases (gpio_out_set, gpio_oe_set) so the writes
// don't clobber other pins' state with a read-modify-write.
#define PIN_MASK ((1u << 2) | (1u << 3) | (1u << 4))

void gpio_234_output_high(void) {
    sio_hw->gpio_oe_set = PIN_MASK;
    sio_hw->gpio_out_set = PIN_MASK;
}
