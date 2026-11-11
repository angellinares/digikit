/* <dk/core/bytes.h>: big-endian byte access for protocol and file fields. Hand-written, header only. */
#ifndef DK_BYTES_H
#define DK_BYTES_H
#include <stdint.h>

/* Store V at P, most significant byte first. */
static inline void dk_bytes_put32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

/* Load the 4 bytes at P, most significant byte first. */
static inline uint32_t dk_bytes_get32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3];
}
#endif
