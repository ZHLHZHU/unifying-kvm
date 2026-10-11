#ifndef RP2040_KVM_BRIDGE_H
#define RP2040_KVM_BRIDGE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
void rp_app_init(void);
void rp_app_task(uint64_t now_us);
void rp_hid_mount(uint8_t dev, uint8_t instance, uint16_t vid, uint16_t pid,
                  uint8_t protocol, const uint8_t *descriptor, uint16_t len);
void rp_hid_report(uint8_t dev, uint8_t instance, const uint8_t *data, uint16_t len);
void rp_hid_unmount(uint8_t dev, uint8_t instance);
size_t bridge_uart_read(uint8_t *out, size_t capacity);
size_t bridge_uart_write(const uint8_t *data, size_t len);
/* bit0 framing, bit1 hardware overrun, bit2 RX ring overflow */
uint32_t bridge_uart_take_errors(void);
bool bridge_hid_receive(uint8_t dev, uint8_t instance);
bool bridge_hid_set_protocol(uint8_t dev, uint8_t instance, uint8_t protocol);
/* 0 not submitted, 1 in flight, 2 completed, 3 failed */
uint8_t bridge_hid_protocol_state(uint8_t dev, uint8_t instance);
/* Board RGB, bit0 R / bit1 G / bit2 B set = lit (GP16/17/18, common anode). */
void bridge_rgb(uint8_t mask);
void bridge_diagnostic(uint32_t code, uint32_t detail);
void bridge_reboot(void);
void bridge_init(void);
#endif
