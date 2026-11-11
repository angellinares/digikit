/* Host test of src/cf/dt2/pdw_hash.h: the vectors are packages/dksample's hashlittle (content hash, seed 0x43fa243a) of the bytes
 * (i * 7 + (i >> 8)) & 255, generated once from dksample. Every vector is also fed in odd pieces, and the 32 KiB pieces of the adapter's
 * readback. */
#include <stdio.h>
#include <stdlib.h>
#include "cf/dt2/pdw_hash.h"

static const struct { uint32_t n, hash; } vec[] = {
    { 1, 0x525aa0e6u }, { 2, 0x08fa0133u }, { 3, 0x2607e71bu }, { 4, 0x3ffcef06u }, { 5, 0xca9e001au },
    { 8, 0x6893d19fu }, { 9, 0xd2f860bfu }, { 11, 0x9a30e701u }, { 12, 0xb75dd23bu }, { 13, 0xc2bc6841u },
    { 16, 0xb44068d3u }, { 17, 0x046cbbe6u }, { 24, 0x969dc6ecu }, { 25, 0x3fa78d9du }, { 28, 0x0ad5f2d0u },
    { 36, 0x6588eb9bu }, { 37, 0x7cfe0046u }, { 100, 0x3b4ac3f9u }, { 1199, 0xd502f734u }, { 1200, 0x1ab0427du },
    { 1201, 0xb7d57f69u }, { 1212, 0x1002d59eu }, { 4096, 0x98e1bb9cu }, { 32767, 0xccb35558u }, { 32768, 0xc5cef572u },
    { 32769, 0x26d83376u }, { 32784, 0x71502b49u }, { 70016, 0xe70f17ecu },
};

static uint8_t data[70016];

static uint32_t fed(uint32_t n, uint32_t piece)
{
    struct pdw_hash h;
    uint32_t at = 0;
    pdw_hash_init(&h);
    while (at < n) {
        uint32_t k = n - at < piece ? n - at : piece;
        pdw_hash_update(&h, data + at, k);
        at += k;
    }
    return pdw_hash_finish(&h);
}

int main(void)
{
    static const uint32_t pieces[] = { 1, 3, 7, 12, 13, 100, 4096, 32768, 70016 };
    uint32_t i, j, bad = 0;
    for (i = 0; i < sizeof data; i++) data[i] = (uint8_t)((i * 7u + (i >> 8)) & 255u);
    for (i = 0; i < sizeof vec / sizeof vec[0]; i++)
        for (j = 0; j < sizeof pieces / sizeof pieces[0]; j++) {
            uint32_t got = fed(vec[i].n, pieces[j]);
            if (got != vec[i].hash) {
                fprintf(stderr, "FAIL n=%u piece=%u got %08x want %08x\n", vec[i].n, pieces[j], got, vec[i].hash);
                bad = 1;
            }
        }
    return bad;
}
