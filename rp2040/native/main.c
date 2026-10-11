#include <stdio.h>
#include "pico/stdlib.h"
#include "bsp/board_api.h"
#include "tusb.h"
#include "bridge.h"
extern volatile uint32_t rp_debug_task_count;
void bridge_diagnostics_task(void);
bool bridge_ota_task(void);

int main(void) {
    board_init();
    /* Custom PCB: active-high TPS2553 enable, open-drain fault, common-anode RGB. */
    gpio_init(14);
    gpio_put(14, 0);
    gpio_set_dir(14, GPIO_OUT);
    gpio_init(15);
    gpio_set_dir(15, GPIO_IN);
    gpio_pull_up(15);
    for (uint pin = 16; pin <= 18; ++pin) {
        gpio_init(pin);
        gpio_put(pin, 1);
        gpio_set_dir(pin, GPIO_OUT);
    }
    gpio_put(14, 1);
    stdio_init_all();
    bridge_init();
#ifdef RP_UART_SMOKE
    printf("rp2040-kvm/1 UART SMOKE board=pico sdk=2.2.0 USB-host=0 UART1=1000000 8N1 TX=GP4 RX=GP5\r\n");
#else
    printf("rp2040-kvm/1 board=pico sdk=2.2.0 USB-host=0 UART1=1000000 8N1 TX=GP4 RX=GP5\r\n");
#endif
    const tusb_rhport_init_t host = { .role = TUSB_ROLE_HOST, .speed = TUSB_SPEED_FULL };
    if (!tuh_rhport_init(0, &host)) {
        printf("FAULT: USB host initialization\r\n");
        for (;;) tight_loop_contents();
    }
    if (board_init_after_tusb) board_init_after_tusb();
    rp_app_init();
    for (;;) {
        if (bridge_ota_task()) continue;
        tuh_task();
        rp_app_task(time_us_64());
        rp_debug_task_count++;
        bridge_diagnostics_task();
    }
}
