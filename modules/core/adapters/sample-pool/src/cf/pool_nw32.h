/* Address helpers of the sample pool that every device shares (no firmware access, no device model). pool_core.h (DT2) and pool_alloc.h
 * (DN2) include it; tests/patchkit/test_sample_pool_core.py and test_sample_pool_dn2.py compile them on the host. */
#ifndef SP_NW32_H
#define SP_NW32_H
#include <stdint.h>

#define SP_NW32_BASE 0x10000000u  /* NW32 alias of the DSP's DDR; physical byte = 0x80000000 + 4 * (w - 0x10000000) (finding 99) */
#define SP_NATIVE_RATE 48000u     /* every stock native file is 48 kHz: the writer sets it (finding 81) and the pool has no other rate */
#define SP_DDR_PHYS 0x80000000u
#define SP_DDR_PHYS_END 0xa0000000u

/* The DSP's normal-word address of a physical DDR byte address, or 0 when it has none (outside the alias or not word aligned). */
static inline uint32_t sp_nw32(uint32_t phys)
{
    if (phys < SP_DDR_PHYS || phys >= SP_DDR_PHYS_END || (phys & 3u))
        return 0;
    return SP_NW32_BASE + ((phys - SP_DDR_PHYS) >> 2);
}

#endif
