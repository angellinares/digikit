/* The +Drive content hash of the firmware (its close hash FUN_4015b1e8: lookup3 over big-endian words, all three state words start at the
 * seed 0x43fa243a, no length in the seed, the last 1 to 12 bytes are the tail), as a streaming hash. The same function as
 * modules/core/runtime/src/rs/src/hash32.rs and packages/dksample/src/hash.rs; tests/pdw-hash.test.ts checks this file against vectors of dksample.
 * Plain C with no firmware include, so that a host test can compile it. */
#ifndef PDW_HASH_H
#define PDW_HASH_H

#include <stdint.h>

#define PDW_HASH_SEED 0x43fa243au

struct pdw_hash {
    uint32_t a, b, c;
    uint32_t n; /* bytes carried in buf (0 to 12) */
    uint8_t buf[12];
};

static inline uint32_t pdw_rot(uint32_t x, uint32_t k)
{
    return (x << k) | (x >> (32u - k));
}

static inline void pdw_hash_init(struct pdw_hash *h)
{
    h->a = h->b = h->c = PDW_HASH_SEED;
    h->n = 0;
}

static inline uint32_t pdw_word(const uint8_t *p)
{
    if (((uintptr_t)p & 3u) == 0u) {
        /* the ColdFire is big-endian; a host test builds this with the byte path */
#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
        return *(const uint32_t *)p;
#endif
    }
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static inline void pdw_hash_block(struct pdw_hash *h, const uint8_t *p)
{
    uint32_t a = h->a + pdw_word(p), b = h->b + pdw_word(p + 4), c = h->c + pdw_word(p + 8);
    a -= c;
    a ^= pdw_rot(c, 4);
    c += b;
    b -= a;
    b ^= pdw_rot(a, 6);
    a += c;
    c -= b;
    c ^= pdw_rot(b, 8);
    b += a;
    a -= c;
    a ^= pdw_rot(c, 16);
    c += b;
    b -= a;
    b ^= pdw_rot(a, 19);
    a += c;
    c -= b;
    c ^= pdw_rot(b, 4);
    b += a;
    h->a = a;
    h->b = b;
    h->c = c;
}

/* `n` (0 to 4) bytes at `p` as the high bytes of a big-endian word. */
static inline uint32_t pdw_tail(const uint8_t *p, uint32_t n)
{
    uint32_t w = 0, i;
    for (i = 0; i < n; i++)
        w |= (uint32_t)p[i] << (24u - 8u * i);
    return w;
}

/* Feed `n` bytes. A block is mixed only when more than 12 bytes are known, so that the last 1 to 12 bytes stay for the tail. */
static inline void pdw_hash_update(struct pdw_hash *h, const uint8_t *p, uint32_t n)
{
    uint32_t have = h->n, i;
    if (n == 0)
        return;
    if (have + n <= 12u) {
        for (i = 0; i < n; i++)
            h->buf[have + i] = p[i];
        h->n = have + n;
        return;
    }
    if (have > 0) {
        uint32_t take = 12u - have;
        for (i = 0; i < take; i++)
            h->buf[have + i] = p[i];
        pdw_hash_block(h, h->buf);
        p += take;
        n -= take;
    }
    while (n > 12u) {
        pdw_hash_block(h, p);
        p += 12;
        n -= 12;
    }
    for (i = 0; i < n; i++)
        h->buf[i] = p[i];
    h->n = n;
}

/* The hash of everything fed (the firmware's value before the `| 1` that it stores). */
static inline uint32_t pdw_hash_finish(const struct pdw_hash *h)
{
    uint32_t a = h->a, b = h->b, c = h->c, n = h->n;
    const uint8_t *p = h->buf;
    if (n == 0)
        return c;
    if (n <= 4u) {
        a += pdw_tail(p, n);
    } else if (n <= 8u) {
        a += pdw_word(p);
        b += pdw_tail(p + 4, n - 4u);
    } else {
        a += pdw_word(p);
        b += pdw_word(p + 4);
        c += pdw_tail(p + 8, n - 8u);
    }
    c ^= b;
    c -= pdw_rot(b, 14);
    a ^= c;
    a -= pdw_rot(c, 11);
    b ^= a;
    b -= pdw_rot(a, 25);
    c ^= b;
    c -= pdw_rot(b, 16);
    a ^= c;
    a -= pdw_rot(c, 4);
    b ^= a;
    b -= pdw_rot(a, 14);
    c ^= b;
    c -= pdw_rot(b, 24);
    return c;
}

#endif
