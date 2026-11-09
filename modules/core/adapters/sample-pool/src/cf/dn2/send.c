/* core/sample-pool on the Digitone II: our own LP0 page sender (docs/design/dn2-sample-pool.md section 2.2).
 *
 * It does what the stock sender `0x400cfb0e` does between its mutex and its timer wait, and nothing else: a tag word and 1,024 data words,
 * each through the stock word helper `0x400cf2dc` (the one the stock sender uses for the tag, and for the data with a nonzero format
 * argument, which is the mode the emulator proof used). The helper writes 0x0080 to the command half, polls bit 0 of the status half, then
 * writes the bytes of the word as it lies in memory in the order b3 b2 b1 b0 (least significant first). Each byte is two halves: `byte << 8 | 0x80`,
 * then `byte << 8`. Left out: the stock mutex, the 100-unit timer wait after the last word (our acknowledgement replaces it) and the unbounded
 * ready poll: a poll that does not end in SP_PORT_POLLS reads stops the page and reports it, so no task can hang on the port.
 *
 * Task context. The port halves are the same two addresses the stock helpers use; no other code writes them (docs/findings/92). The host test
 * (tests/patchkit/test_sample_pool_dn2.py) defines SP_PORT_HOST and supplies the three access functions as a model of the port. */

#include <stdint.h>
#include "cf/dn2/xfer.h"

typedef uint32_t u32;
typedef uint8_t u8;

#ifdef SP_PORT_HOST
extern void sp_port_cmd(uint16_t v);
extern uint16_t sp_port_status(void);
extern void sp_port_data(uint16_t v);
#else
static inline void sp_port_cmd(uint16_t v) { *(volatile uint16_t *)0x8c00000au = v; }
static inline uint16_t sp_port_status(void) { return *(volatile uint16_t *)0x8c000002u; }
static inline void sp_port_data(uint16_t v) { *(volatile uint16_t *)0x8c000002u = v; }
#endif

#define SP_PORT_POLLS 8192u   /* readiness reads per word before the page is given up */

/* One word, given as its bytes in memory order (b0 first). 0, or -1 when the port was not ready in time. */
static int send_word(u32 b0, u32 b1, u32 b2, u32 b3)
{
    u32 n = 0;
    sp_port_cmd(0x80);
    while (!(sp_port_status() & 1u))
        if (++n >= SP_PORT_POLLS)
            return -1;
    sp_port_data((uint16_t)((b3 << 8) | 0x80u));
    sp_port_data((uint16_t)(b3 << 8));
    sp_port_data((uint16_t)((b2 << 8) | 0x80u));
    sp_port_data((uint16_t)(b2 << 8));
    sp_port_data((uint16_t)((b1 << 8) | 0x80u));
    sp_port_data((uint16_t)(b1 << 8));
    sp_port_data((uint16_t)((b0 << 8) | 0x80u));
    sp_port_data((uint16_t)(b0 << 8));
    return 0;
}

int sp_raw_send(u32 tag, const u8 *page)
{
    u32 i;
    if (send_word(tag >> 24, (tag >> 16) & 0xffu, (tag >> 8) & 0xffu, tag & 0xffu))
        return -1;
    for (i = 0; i < SPA_PAGE_BYTES; i += 4)
        if (send_word(page[i], page[i + 1], page[i + 2], page[i + 3]))
            return -1;
    return 0;
}
