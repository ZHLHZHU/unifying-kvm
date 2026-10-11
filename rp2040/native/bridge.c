#include "bridge.h"
#include <stdio.h>
#include <string.h>
#include "pico/stdlib.h"
#include "hardware/irq.h"
#include "hardware/uart.h"
#include "hardware/sync.h"
#include "hardware/watchdog.h"
#include "tusb.h"

#define RX_SIZE 1024u
#define HID_COUNT 8u
static uint8_t rx_ring[RX_SIZE];
static volatile uint16_t rx_head, rx_tail;
static volatile uint32_t uart_errors;
static uint8_t hid_dev[HID_COUNT], protocol_state[HID_COUNT];
struct diagnostic { uint32_t code, detail; };
static struct diagnostic diagnostics[32];
static uint8_t diagnostic_head, diagnostic_tail;
/* SWD-readable diagnostics also work without a UART0 adapter. Not RF proof. */
volatile uint32_t rp_debug_last_code, rp_debug_last_detail;
volatile uint32_t rp_debug_state, rp_debug_fault;
volatile uint32_t rp_debug_task_count;
volatile uint32_t rp_debug_hid_reports[HID_COUNT];
volatile uint32_t rp_debug_hid_report_len[HID_COUNT];
volatile uint8_t rp_debug_hid_report[HID_COUNT][8];

static void uart_rx_irq(void) {
    while (uart_is_readable(uart1)) {
        uint32_t dr = uart_get_hw(uart1)->dr;
        if (dr & (7u << 8)) uart_errors |= 1u;
        if (dr & (1u << 11)) uart_errors |= 2u;
        uint16_t next = (rx_head + 1u) & (RX_SIZE - 1u);
        if (next == rx_tail) uart_errors |= 4u;
        else { rx_ring[rx_head] = (uint8_t)dr; rx_head = next; }
    }
    uart_get_hw(uart1)->rsr = 0;
}

void bridge_init(void) {
    /* Set idle and mux before enabling RX to avoid injecting a startup byte. */
    gpio_pull_up(5);
    gpio_set_function(4, GPIO_FUNC_UART);
    gpio_set_function(5, GPIO_FUNC_UART);
    uart_init(uart1, 1000000);
    uart_set_hw_flow(uart1, false, false);
    uart_set_format(uart1, 8, 1, UART_PARITY_NONE);
    uart_set_fifo_enabled(uart1, true);
    irq_set_exclusive_handler(UART1_IRQ, uart_rx_irq);
    irq_set_enabled(UART1_IRQ, true);
    uart_set_irq_enables(uart1, true, false);
}

size_t bridge_uart_read(uint8_t *out, size_t capacity) {
    if (!out) return 0;
    size_t n = 0;
    while (n < capacity && rx_tail != rx_head) {
        out[n++] = rx_ring[rx_tail];
        rx_tail = (rx_tail + 1u) & (RX_SIZE - 1u);
    }
    return n;
}

size_t bridge_uart_write(const uint8_t *data, size_t len) {
    if (!data) return 0;
    size_t n = 0;
    while (n < len && uart_is_writable(uart1)) uart_get_hw(uart1)->dr = data[n++];
    return n;
}

uint32_t bridge_uart_take_errors(void) {
    uint32_t flags = save_and_disable_interrupts();
    uint32_t errors = uart_errors;
    uart_errors = 0;
    restore_interrupts(flags);
    return errors;
}

static bool valid_hid(uint8_t dev, uint8_t instance) {
    return instance < HID_COUNT && hid_dev[instance] == dev && dev != 0;
}

bool bridge_hid_receive(uint8_t dev, uint8_t instance) {
    return valid_hid(dev, instance) && tuh_hid_receive_report(dev, instance);
}

static void protocol_complete(tuh_xfer_t *xfer) {
    uint8_t instance = (uint8_t)xfer->user_data;
    if (valid_hid(xfer->daddr, instance))
        protocol_state[instance] = xfer->result == XFER_RESULT_SUCCESS ? 2 : 3;
}

bool bridge_hid_set_protocol(uint8_t dev, uint8_t instance, uint8_t protocol) {
    tuh_itf_info_t info;
    if (!valid_hid(dev, instance) || protocol > 1 || protocol_state[instance] == 1 ||
        !tuh_hid_itf_get_info(dev, instance, &info) || !info.desc.bInterfaceSubClass) return false;
    /* TinyUSB's HID callback cannot distinguish a failed same-mode request.
       Use its public control API so only actual successful completion opens decoding. */
    tusb_control_request_t request = {
        .bmRequestType = 0x21, .bRequest = HID_REQ_CONTROL_SET_PROTOCOL,
        .wValue = protocol, .wIndex = info.desc.bInterfaceNumber, .wLength = 0
    };
    tuh_xfer_t xfer = { .daddr = dev, .ep_addr = 0, .setup = &request,
        .buffer = NULL, .complete_cb = protocol_complete, .user_data = instance };
    protocol_state[instance] = 1;
    if (!tuh_control_xfer(&xfer)) { protocol_state[instance] = 0; return false; }
    return true;
}

uint8_t bridge_hid_protocol_state(uint8_t dev, uint8_t instance) {
    return valid_hid(dev, instance) ? protocol_state[instance] : 0;
}

void bridge_rgb(uint8_t mask) {
    /* Common anode: driving the cathode low lights the color. */
    gpio_put(16, !(mask & 1u));
    gpio_put(17, !(mask & 2u));
    gpio_put(18, !(mask & 4u));
}

void tuh_hid_mount_cb(uint8_t dev, uint8_t instance, const uint8_t *descriptor, uint16_t len) {
    if (instance >= HID_COUNT) return;
    hid_dev[instance] = dev;
    protocol_state[instance] = 0;
    uint16_t vid = 0, pid = 0;
    tuh_vid_pid_get(dev, &vid, &pid);
    rp_hid_mount(dev, instance, vid, pid, tuh_hid_interface_protocol(dev, instance), descriptor, len);
}

void tuh_hid_umount_cb(uint8_t dev, uint8_t instance) {
    if (!valid_hid(dev, instance)) return;
    hid_dev[instance] = 0;
    protocol_state[instance] = 0;
    rp_hid_unmount(dev, instance);
}

void tuh_hid_report_received_cb(uint8_t dev, uint8_t instance, const uint8_t *data, uint16_t len) {
    if (instance < HID_COUNT) {
        rp_debug_hid_reports[instance]++;
        rp_debug_hid_report_len[instance] = len;
        for (unsigned i = 0; i < 8 && i < len; ++i) rp_debug_hid_report[instance][i] = data[i];
    }
    rp_hid_report(dev, instance, data, len);
}


void bridge_diagnostic(uint32_t code, uint32_t detail) {
    rp_debug_last_code = code;
    rp_debug_last_detail = detail;
    if (code == 5) rp_debug_state = detail;
    if (code == 6) rp_debug_fault = detail + 1u;
    uint8_t next = (diagnostic_head + 1u) % 32u;
    if (next != diagnostic_tail) {
        diagnostics[diagnostic_head] = (struct diagnostic){ code, detail };
        diagnostic_head = next;
    }
}

void bridge_diagnostics_task(void) {
    static char line[128];
    static size_t length, offset;
    if (offset == length) {
        if (diagnostic_tail == diagnostic_head) return;
        struct diagnostic event = diagnostics[diagnostic_tail];
        diagnostic_tail = (diagnostic_tail + 1u) % 32u;
        static const char *const states[] = {
            "BOOT_VER", "BOOT_SELECT", "BOOT_RELEASE", "READY", "RELEASE_OLD",
            "SELECT_NEW", "RELEASE_NEW", "RECOVER_VER", "RECOVER_STATUS", "RECOVER_OLD",
            "RECOVER_SELECT", "RECOVER_NEW", "WAIT_RELEASE", "UNPLUG_RELEASE", "FAULT", "MAINTENANCE",
            "PAIR_RELEASE", "PAIRING", "PAIR_DONE"
        };
        static const char *const faults[] = {
            "UART timeout", "UART error", "RX overflow", "FIFO overflow",
            "response error", "reserved", "keyboard report error"
        };
        int n;
        if (event.code == 5 && event.detail < sizeof(states) / sizeof(states[0]))
            n = snprintf(line, sizeof(line), "STATE %s\r\n", states[event.detail]);
        else if (event.code == 6 && event.detail < sizeof(faults) / sizeof(faults[0]))
            n = snprintf(line, sizeof(line), "FAULT: %s\r\n", faults[event.detail]);
        else if (event.code == 2)
            n = snprintf(line, sizeof(line), "USB keyboard mount dev=%lu interface=%lu\r\n", (unsigned long)(event.detail >> 8), (unsigned long)(event.detail & 255));
        else if (event.code == 3)
            n = snprintf(line, sizeof(line), "USB keyboard protocol=%s\r\n", event.detail ? "Report" : "Boot");
        else if (event.code == 4)
            n = snprintf(line, sizeof(line), "Scroll Lock rising\r\n");
        else if (event.code == 13)
            n = snprintf(line, sizeof(line), "slot %lu READY\r\n", (unsigned long)event.detail);
        else
            n = snprintf(line, sizeof(line), "KVM event=%lu detail=%lu\r\n", (unsigned long)event.code, (unsigned long)event.detail);
        length = n > 0 ? ((size_t)n < sizeof(line) ? (size_t)n : sizeof(line) - 1u) : 0;
        offset = 0;
    }
    /* UART0 FIFO only: diagnostic backpressure never delays USB/UART1. */
    for (size_t budget = 32; budget && offset < length && uart_is_writable(uart0); --budget)
        uart_get_hw(uart0)->dr = (uint8_t)line[offset++];
}

void bridge_reboot(void) {
    watchdog_reboot(0, 0, 0);
    for (;;) tight_loop_contents();
}
