/*
 * The MIT License (MIT)
 *
 * Copyright (c) 2020 Raspberry Pi (Trading) Ltd.
 * Copyright (c) 2021 Ha Thach (tinyusb.org)
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
 * THE SOFTWARE.
 */
#include "host/hcd.h"
#include "host/usbh.h"
#include "rp2040_usb.h"

/* RP2040 EP1..15 cannot ACK a repeated IN DATA PID, and then retry forever.
 * EPX ACKs/discards it correctly. Use EPX for every endpoint, one handshake
 * per interrupt; SOF schedules NAK'd endpoints without starving other traffic.
 * Silicon discussion: https://github.com/raspberrypi/pico-feedback/issues/394
 */
#define EP_COUNT USB_MAX_ENDPOINTS
#define DEV_COUNT (CFG_TUH_DEVICE_MAX + CFG_TUH_HUB + 1)
#define SIE_BASE (USB_SIE_CTRL_SOF_EN_BITS | USB_SIE_CTRL_KEEP_ALIVE_EN_BITS | \
                  USB_SIE_CTRL_PULLDOWN_EN_BITS | USB_SIE_CTRL_EP0_INT_1BUF_BITS)
#define SIE_RESULT_BITS (USB_SIE_STATUS_ACK_REC_BITS | USB_SIE_STATUS_NAK_REC_BITS | \
                         USB_SIE_STATUS_STALL_REC_BITS | USB_SIE_STATUS_RX_TIMEOUT_BITS | \
                         USB_SIE_STATUS_DATA_SEQ_ERROR_BITS | USB_SIE_STATUS_TRANS_COMPLETE_BITS)

struct endpoint {
    uint8_t dev, addr, pid;
    bool configured, pending, setup;
    uint16_t mps, total, done, interval_ms;
    uint32_t due;
    uint8_t *buffer;
};
static struct endpoint endpoints[EP_COUNT];
static uint16_t ep0_mps[DEV_COUNT];
static struct endpoint *running;
static uint8_t setup_bytes[8];
static uint16_t packet_len;
static uint8_t cursor;

static bool lock_usb(void) {
    bool enabled = irq_is_enabled(USBCTRL_IRQ);
    irq_set_enabled(USBCTRL_IRQ, false);
    return enabled;
}
static void unlock_usb(bool enabled) { irq_set_enabled(USBCTRL_IRQ, enabled); }

static struct endpoint *find_endpoint(uint8_t dev, uint8_t addr) {
    if (!tu_edpt_number(addr)) return &endpoints[0];
    for (unsigned i = 1; i < EP_COUNT; ++i)
        if (endpoints[i].configured && endpoints[i].dev == dev && endpoints[i].addr == addr)
            return &endpoints[i];
    return NULL;
}

static void clear_result(void) {
    usb_hw_clear->sie_status = SIE_RESULT_BITS;
    usb_hw_clear->buf_status = 3;
    usbh_dpram->epx_buf_ctrl = 0;
    /* DPRAM and SIE are clocked at 48MHz, not at the CPU clock. */
    busy_wait_us_32(1);
}

static void stop_running(void) {
    if (running) {
        usb_hw_set->sie_ctrl = USB_SIE_CTRL_STOP_TRANS_BITS;
        /* A full-speed 64-byte packet plus handshake fits inside this bound. */
        busy_wait_us_32(100);
        running = NULL;
    }
    usb_hw->sie_ctrl = SIE_BASE;
    clear_result();
}

static void start_packet(struct endpoint *ep) {
    running = ep;
    clear_result();
    bool in = tu_edpt_dir(ep->addr) == TUSB_DIR_IN;
    uint32_t flags = SIE_BASE;
    if (hcd_port_speed_get(0) != tuh_speed_get(ep->dev)) flags |= USB_SIE_CTRL_PREAMBLE_EN_BITS;
    usb_hw->dev_addr_ctrl = ep->dev | ((uint32_t)tu_edpt_number(ep->addr) << USB_ADDR_ENDP_ENDPOINT_LSB);
    /* Interrupt type makes EPX stop after a single handshake, including NAK.
     * It does not change the endpoint type advertised by the device. */
    usbh_dpram->epx_ctrl = EP_CTRL_ENABLE_BITS | EP_CTRL_INTERRUPT_PER_BUFFER |
                          (TUSB_XFER_INTERRUPT << EP_CTRL_BUFFER_TYPE_LSB) |
                          hw_data_offset(usbh_dpram->epx_data);
    if (ep->setup) {
        packet_len = 8;
        for (unsigned i = 0; i < 8; ++i) usbh_dpram->setup_packet[i] = setup_bytes[i];
        flags |= USB_SIE_CTRL_SEND_SETUP_BITS;
    } else {
        packet_len = tu_min16(ep->total - ep->done, ep->mps);
        uint32_t ctrl = packet_len | USB_BUF_CTRL_AVAIL | USB_BUF_CTRL_LAST | USB_BUF_CTRL_SEL;
        if (ep->pid) ctrl |= USB_BUF_CTRL_DATA1_PID;
        if (!in) {
            for (unsigned i = 0; i < packet_len; ++i)
                usbh_dpram->epx_data[i] = ep->buffer[ep->done + i];
            ctrl |= USB_BUF_CTRL_FULL;
        }
        usbh_dpram->epx_buf_ctrl = ctrl & ~USB_BUF_CTRL_AVAIL;
        busy_wait_us_32(1);
        usbh_dpram->epx_buf_ctrl = ctrl;
        flags |= in ? USB_SIE_CTRL_RECEIVE_DATA_BITS : USB_SIE_CTRL_SEND_DATA_BITS;
    }
    usb_hw->sie_ctrl = flags;
    busy_wait_us_32(1);
    usb_hw->sie_ctrl = flags | USB_SIE_CTRL_START_TRANS_BITS;
}

static void schedule(void) {
    if (running || !hcd_port_connect_status(0)) return;
    uint32_t now = time_us_32();
    if (endpoints[0].pending && (int32_t)(now - endpoints[0].due) >= 0) {
        start_packet(&endpoints[0]);
        return;
    }
    for (unsigned n = 0; n < EP_COUNT - 1; ++n) {
        cursor = cursor == EP_COUNT - 1 ? 1 : cursor + 1;
        struct endpoint *ep = &endpoints[cursor];
        if (ep->pending && (int32_t)(now - ep->due) >= 0) {
            start_packet(ep);
            return;
        }
    }
}

static void complete(struct endpoint *ep, xfer_result_t result) {
    uint8_t dev = ep->dev, addr = ep->addr;
    uint16_t done = ep->done;
    ep->pending = false;
    ep->setup = false;
    ep->buffer = NULL;
    hcd_event_xfer_complete(dev, addr, done, result, true);
}

static void packet_complete(uint32_t status, uint32_t buffers) {
    struct endpoint *ep = running;
    uint32_t ctrl = usbh_dpram->epx_buf_ctrl;
    running = NULL;
    clear_result();
    if (!ep) return;
    ep->due = time_us_32() + (uint32_t)ep->interval_ms * 1000u;
    if (status & USB_SIE_STATUS_STALL_REC_BITS) {
        complete(ep, XFER_RESULT_STALLED);
    } else if (status & USB_SIE_STATUS_RX_TIMEOUT_BITS) {
        complete(ep, XFER_RESULT_FAILED);
    } else if (status & USB_SIE_STATUS_DATA_SEQ_ERROR_BITS) {
        /* EPX already ACK'd the duplicate. Do not copy it or toggle our PID.
         * The next packet should carry the PID we were already expecting. */
        if (!tu_edpt_dir(ep->addr) || ep->setup) complete(ep, XFER_RESULT_FAILED);
    } else if (status & USB_SIE_STATUS_NAK_REC_BITS) {
        /* Leave the request pending, with the same data PID and user buffer. */
    } else if (ep->setup) {
        ep->done = 8;
        complete(ep, XFER_RESULT_SUCCESS);
    } else if ((buffers & 3) || (status & USB_SIE_STATUS_TRANS_COMPLETE_BITS)) {
        bool in = tu_edpt_dir(ep->addr) == TUSB_DIR_IN;
        uint16_t len = ctrl & USB_BUF_CTRL_LEN_MASK;
        if (len > packet_len || (in && !(ctrl & USB_BUF_CTRL_FULL))) {
            complete(ep, XFER_RESULT_FAILED);
            return;
        }
        if (in)
            for (unsigned i = 0; i < len; ++i)
                ep->buffer[ep->done + i] = usbh_dpram->epx_data[i];
        ep->done += len;
        ep->pid ^= 1u;
        if (ep->done == ep->total || len < ep->mps) complete(ep, XFER_RESULT_SUCCESS);
    }
}

static void host_irq(void) {
    uint32_t ints = usb_hw->ints;
    uint32_t status = usb_hw->sie_status;
    uint32_t buffers = usb_hw->buf_status;
    if (ints & USB_INTS_HOST_CONN_DIS_BITS) {
        usb_hw_clear->sie_status = USB_SIE_STATUS_SPEED_BITS;
        if (status & USB_SIE_STATUS_SPEED_BITS) {
            hcd_event_device_attach(0, true);
        } else {
            stop_running();
            memset(endpoints, 0, sizeof(endpoints));
            hcd_event_device_remove(0, true);
            return;
        }
    }
    if (ints & USB_INTS_HOST_RESUME_BITS) usb_hw_clear->sie_status = USB_SIE_STATUS_RESUME_BITS;
    if (ints & (USB_INTS_BUFF_STATUS_BITS | USB_INTS_TRANS_COMPLETE_BITS |
                USB_INTS_STALL_BITS | USB_INTS_ERROR_DATA_SEQ_BITS | USB_INTS_ERROR_RX_TIMEOUT_BITS))
        packet_complete(status, buffers);
    if (ints & USB_INTS_HOST_SOF_BITS) (void)usb_hw->sof_rd;
    schedule();
}

void hcd_int_handler(uint8_t rhport, bool in_isr) { (void)rhport; (void)in_isr; host_irq(); }

bool hcd_init(uint8_t rhport, const tusb_rhport_init_t *rh_init) {
    (void)rh_init;
    TU_ASSERT(rhport == 0);
    rp2040_usb_init();
    memset(endpoints, 0, sizeof(endpoints));
    memset(ep0_mps, 0, sizeof(ep0_mps));
    running = NULL;
    cursor = 0;
    usb_hw->pwr = USB_USB_PWR_VBUS_DETECT_BITS | USB_USB_PWR_VBUS_DETECT_OVERRIDE_EN_BITS;
    irq_remove_handler(USBCTRL_IRQ, host_irq);
    irq_add_shared_handler(USBCTRL_IRQ, host_irq, PICO_SHARED_IRQ_HANDLER_HIGHEST_ORDER_PRIORITY);
    usb_hw->main_ctrl = USB_MAIN_CTRL_CONTROLLER_EN_BITS | USB_MAIN_CTRL_HOST_NDEVICE_BITS;
    usb_hw->sie_ctrl = SIE_BASE;
    usb_hw->int_ep_ctrl = 0;
    usb_hw->inte = USB_INTE_BUFF_STATUS_BITS | USB_INTE_HOST_CONN_DIS_BITS |
                   USB_INTE_HOST_RESUME_BITS | USB_INTE_STALL_BITS |
                   USB_INTE_TRANS_COMPLETE_BITS | USB_INTE_ERROR_RX_TIMEOUT_BITS |
                   USB_INTE_ERROR_DATA_SEQ_BITS | USB_INTE_HOST_SOF_BITS;
    return true;
}

bool hcd_deinit(uint8_t rhport) {
    (void)rhport;
    irq_set_enabled(USBCTRL_IRQ, false);
    usb_hw->inte = 0;
    irq_remove_handler(USBCTRL_IRQ, host_irq);
    reset_block(RESETS_RESET_USBCTRL_BITS);
    unreset_block_wait(RESETS_RESET_USBCTRL_BITS);
    return true;
}
void hcd_int_enable(uint8_t rhport) { (void)rhport; irq_set_enabled(USBCTRL_IRQ, true); }
void hcd_int_disable(uint8_t rhport) { (void)rhport; irq_set_enabled(USBCTRL_IRQ, false); }
uint32_t hcd_frame_number(uint8_t rhport) { (void)rhport; return usb_hw->sof_rd; }
bool hcd_port_connect_status(uint8_t rhport) {
    (void)rhport;
    return (usb_hw->sie_status & USB_SIE_STATUS_SPEED_BITS) != 0;
}
tusb_speed_t hcd_port_speed_get(uint8_t rhport) {
    (void)rhport;
    return ((usb_hw->sie_status & USB_SIE_STATUS_SPEED_BITS) >> USB_SIE_STATUS_SPEED_LSB) == 1 ?
           TUSB_SPEED_LOW : TUSB_SPEED_FULL;
}
/* The controller drives the attach reset itself (100ms after attach: 50ms SE0,
 * then SPEED + HOST_CONN_DIS). SIE_CTRL.RESET_BUS raises a fresh CONN_DIS
 * attach, which makes usbh restart enumeration forever. Upstream TinyUSB
 * hcd_rp2040.c leaves these hooks empty for the same reason. */
void hcd_port_reset(uint8_t rhport) { (void)rhport; }
void hcd_port_reset_end(uint8_t rhport) { (void)rhport; }

bool hcd_edpt_open(uint8_t rhport, uint8_t dev, tusb_desc_endpoint_t const *desc) {
    (void)rhport;
    uint16_t mps = tu_edpt_packet_size(desc);
    TU_ASSERT(dev < DEV_COUNT && mps <= 64 && mps != 0);
    bool enabled = lock_usb();
    if (!tu_edpt_number(desc->bEndpointAddress)) {
        ep0_mps[dev] = mps;
        unlock_usb(enabled);
        return true;
    }
    struct endpoint *ep = find_endpoint(dev, desc->bEndpointAddress);
    if (!ep)
        for (unsigned i = 1; i < EP_COUNT; ++i)
            if (!endpoints[i].configured) { ep = &endpoints[i]; break; }
    if (ep && !ep->pending) {
        *ep = (struct endpoint){ .dev = dev, .addr = desc->bEndpointAddress, .mps = mps,
            .configured = true, .interval_ms = desc->bmAttributes.xfer == TUSB_XFER_INTERRUPT ? desc->bInterval : 1 };
    } else {
        unlock_usb(enabled);
        return false;
    }
    unlock_usb(enabled);
    return true;
}

bool hcd_edpt_xfer(uint8_t rhport, uint8_t dev, uint8_t addr, uint8_t *buffer, uint16_t len) {
    (void)rhport;
    TU_ASSERT(dev < DEV_COUNT && (buffer || !len));
    bool enabled = lock_usb();
    struct endpoint *ep = find_endpoint(dev, addr);
    if (!ep || ep->pending) { unlock_usb(enabled); return false; }
    if (!tu_edpt_number(addr)) {
        if (ep->dev != dev || ep->addr != addr) ep->pid = 1;
        ep->dev = dev;
        ep->addr = addr;
        ep->mps = ep0_mps[dev] ? ep0_mps[dev] : 8;
        ep->interval_ms = 1;
        ep->due = time_us_32();
    }
    ep->setup = false;
    ep->buffer = buffer;
    ep->total = len;
    ep->done = 0;
    ep->pending = true;
    schedule();
    unlock_usb(enabled);
    return true;
}

bool hcd_setup_send(uint8_t rhport, uint8_t dev, uint8_t const setup_packet[8]) {
    (void)rhport;
    TU_ASSERT(dev < DEV_COUNT);
    bool enabled = lock_usb();
    struct endpoint *ep = &endpoints[0];
    if (ep->pending) { unlock_usb(enabled); return false; }
    memcpy(setup_bytes, setup_packet, 8);
    *ep = (struct endpoint){ .dev = dev, .addr = 0, .pid = 1, .mps = ep0_mps[dev] ? ep0_mps[dev] : 8,
        .configured = true, .pending = true, .setup = true, .total = 8, .interval_ms = 1, .due = time_us_32() };
    schedule();
    unlock_usb(enabled);
    return true;
}

bool hcd_edpt_abort_xfer(uint8_t rhport, uint8_t dev, uint8_t addr) {
    (void)rhport;
    bool enabled = lock_usb();
    struct endpoint *ep = find_endpoint(dev, addr);
    bool aborted = ep && ep->pending && ep != running && ep->dev == dev;
    if (aborted) { ep->pending = false; ep->buffer = NULL; }
    unlock_usb(enabled);
    return aborted;
}

bool hcd_edpt_clear_stall(uint8_t rhport, uint8_t dev, uint8_t addr) {
    (void)rhport;
    bool enabled = lock_usb();
    struct endpoint *ep = find_endpoint(dev, addr);
    bool cleared = ep && ep->dev == dev && !ep->pending;
    if (cleared) ep->pid = 0;
    unlock_usb(enabled);
    return cleared;
}

void hcd_device_close(uint8_t rhport, uint8_t dev) {
    (void)rhport;
    bool enabled = lock_usb();
    if (running && running->dev == dev) stop_running();
    for (unsigned i = 0; i < EP_COUNT; ++i)
        if (endpoints[i].dev == dev) memset(&endpoints[i], 0, sizeof(endpoints[i]));
    if (dev < DEV_COUNT) ep0_mps[dev] = 0;
    schedule();
    unlock_usb(enabled);
}
