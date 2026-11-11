/* test/c-demo: 64-bit arithmetic on a 32-bit ColdFire. Each operation below is a libgcc helper call (__udivdi3, __divdi3,
 * __umoddi3, __moddi3, __muldi3, __ashldi3, __lshrdi3, __ashrdi3); the build links them from the -mcpu=54418 libgcc.
 *
 * The inputs sit in `c_demo_in` (initialised data, read through volatile) so the compiler cannot fold anything; the
 * results are stored as big-endian word pairs (high, low) in `c_demo_out`, which `dkm_init` logs the address of. */

#include <dk/core/api.h>

typedef uint32_t u32;
typedef unsigned long long u64;
typedef long long s64;

/* [0] udivisor / dividend pair, see dkm_init */
volatile u64 c_demo_in[4] = {
    0xFEDCBA9876543210ULL, /* a */
    0x0000000123456789ULL, /* b */
    0xFFFFFFFF89ABCDEFULL, /* c: negative as s64 */
    0x0000000000003039ULL, /* d = 12345 */
};
volatile u32 c_demo_n[3] = {13, 7, 9};

/* [0] magic, [1] 0, then 9 results as (hi, lo) */
u32 c_demo_out[2 + 2 * 9];

static void put(int i, u64 v)
{
    c_demo_out[2 + 2 * i] = (u32)(v >> 32);
    c_demo_out[3 + 2 * i] = (u32)v;
}

int32_t dkm_init(const dk_api *api)
{
    u64 a = c_demo_in[0], b = c_demo_in[1];
    s64 c = (s64)c_demo_in[2], d = (s64)c_demo_in[3];
    u32 n0 = c_demo_n[0], n1 = c_demo_n[1], n2 = c_demo_n[2];

    put(0, a / b);              /* __udivdi3 */
    put(1, a % b);              /* __umoddi3 */
    put(2, (u64)(c / d));       /* __divdi3 */
    put(3, (u64)(c % d));       /* __moddi3 */
    put(4, a * b);              /* __muldi3 */
    put(5, a << n0);            /* __ashldi3 */
    put(6, a >> n1);            /* __lshrdi3 */
    put(7, (u64)(c >> n2));     /* __ashrdi3 */
    put(8, (u64)(c * d));       /* __muldi3, signed */
    c_demo_out[0] = 0x43444D4F; /* "CDMO" */
    api->log((const uint8_t *)"c-demo results", (u32)c_demo_out);
    return 0;
}
