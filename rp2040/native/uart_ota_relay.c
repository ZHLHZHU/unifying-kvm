#include <string.h>
#include <stdio.h>
#include "pico/stdlib.h"
#include "hardware/watchdog.h"
#include "bridge.h"
/* Host writes payload and length before request. Mode stays exclusive until reset. */
struct ota_mailbox {
    uint32_t magic, version, request, status, length, response_length;
    uint8_t command[256], response[600];
};
volatile struct ota_mailbox rp_ota_mailbox = { .magic=0x4f544131, .version=1 };
static bool exclusive, pending;
static uint32_t number=0x80000000;
static char wire[280], prefix[11];
static size_t send_len, sent, received;
static uint64_t deadline;
static void finish(uint32_t status, const uint8_t *data, size_t len) {
    len = len > 600 ? 600 : len;
    for (size_t i=0;i<len;i++) rp_ota_mailbox.response[i]=data[i];
    rp_ota_mailbox.response_length=len;
    rp_ota_mailbox.request=0;
    __compiler_memory_barrier();
    rp_ota_mailbox.status=status;
    pending=false;
}
bool bridge_ota_task(void) {
    uint32_t request=rp_ota_mailbox.request;
    if (!exclusive) {
        if (request != 1) return false;
        exclusive=true;
        uint8_t junk[128]; while(bridge_uart_read(junk,sizeof(junk))) {}
        bridge_uart_take_errors();
        finish(3,(const uint8_t *)"ENTERED",7);
    }
    if (!pending && request==3) { watchdog_reboot(0,0,10); while(1) tight_loop_contents(); }
    if (!pending && rp_ota_mailbox.request==2) {
        size_t n=rp_ota_mailbox.length;
        if(!n || n>240) {finish(4,(const uint8_t *)"BAD LENGTH",10); return true;}
        char cmd[256];
        for(size_t i=0;i<n;i++) {cmd[i]=rp_ota_mailbox.command[i]; if(cmd[i]=='\n'||cmd[i]=='\r'||cmd[i]<32||cmd[i]>126){finish(4,(const uint8_t *)"BAD COMMAND",11);return true;}}
        cmd[n]=0;
        number++; snprintf(prefix,sizeof(prefix),"@%08lX ",(unsigned long)number);
        send_len=snprintf(wire,sizeof(wire),"\n%s%s\n",prefix,cmd);
        /* UPAIR retries a full channel scan up to 8 times; OTA blocks reply in milliseconds. */
        sent=received=0; deadline=time_us_64()+30000000; pending=true;
        rp_ota_mailbox.response_length=0; rp_ota_mailbox.status=2;
    }
    if(pending) {
        sent+=bridge_uart_write((const uint8_t *)wire+sent,send_len-sent);
        uint8_t b[64];size_t n=bridge_uart_read(b,sizeof(b));
        for(size_t i=0;i<n;i++) {
            if(received>=600){finish(4,(const uint8_t *)"RX OVERFLOW",11);break;}
            rp_ota_mailbox.response[received++]=b[i];
            if(b[i]=='\n') {
                bool match=received>=10;
                for(size_t j=0;j<10 && match;j++) match=rp_ota_mailbox.response[j]==(uint8_t)prefix[j];
                if(match){rp_ota_mailbox.response_length=received;rp_ota_mailbox.request=0;__compiler_memory_barrier();rp_ota_mailbox.status=3;pending=false;break;}
                received=0;
            }
        }
        if(pending && bridge_uart_take_errors()) finish(4,(const uint8_t *)"UART ERROR",10);
        if(pending && time_us_64()>deadline) finish(4,(const uint8_t *)"TIMEOUT",7);
    }
    return true;
}
