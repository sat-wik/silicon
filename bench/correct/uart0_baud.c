#include <stdint.h>

typedef volatile uint32_t io_rw_32;

struct uart_hw_t {
    io_rw_32 uartdr;
    io_rw_32 uartrsr;
    io_rw_32 _pad0[4];
    io_rw_32 uartfr;
    io_rw_32 _pad1;
    io_rw_32 uartilpr;
    io_rw_32 uartibrd;
    io_rw_32 uartfbrd;
    io_rw_32 uartlcr_h;
    io_rw_32 uartcr;
};

#define uart0_hw ((struct uart_hw_t *)0x40034000u)

// Configure UART0 for 115200 baud from a 125MHz peripheral clock.
// Divisor = 125000000 / (16 * 115200) = 67.816...
// Integer part = 67, fractional part = round(0.816 * 64) = 52.
void uart0_set_baud_115200(void) {
    uart0_hw->uartibrd = 67u;
    uart0_hw->uartfbrd = 52u;
}
