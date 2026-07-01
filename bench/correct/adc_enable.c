#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct adc_hw_t {
    io_rw_32 cs;
    io_rw_32 result;
    io_rw_32 fcs;
    io_rw_32 fifo;
    io_rw_32 div;
};

#define adc_hw ((struct adc_hw_t *)0x4004c000u)

// Power on the ADC and select input 0 (GPIO26). The ADC must be enabled
// before any conversion can start or the READY bit is meaningful.
// CS.EN[0] = 1, CS.INPUT_SEL[5:3] = 0 (channel 0).
void adc_init_ch0(void) {
    adc_hw->cs = (1u << 0);
}
