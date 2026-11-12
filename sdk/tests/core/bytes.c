/* Checks the big-endian byte helpers: byte order on the wire, round trip, and no write outside the four bytes. */
#include <assert.h>
#include <dk/core/bytes.h>
int main(void)
{
    uint8_t b[6] = {0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa};
    dk_bytes_put32(b + 1, 0x12345678u);
    assert(b[0] == 0xaa && b[1] == 0x12 && b[2] == 0x34 && b[3] == 0x56 && b[4] == 0x78 &&
           b[5] == 0xaa);
    assert(dk_bytes_get32(b + 1) == 0x12345678u);
    static const uint32_t values[] = {0u, 1u, 0x80u, 0x8000u, 0x80000000u, 0xffffffffu};
    for (unsigned i = 0; i < sizeof(values) / sizeof(values[0]); i++) {
        dk_bytes_put32(b, values[i]);
        assert(dk_bytes_get32(b) == values[i]);
    }
    const uint8_t wire[4] = {0xff, 0x00, 0x00, 0x01};
    assert(dk_bytes_get32(wire) == 0xff000001u);
    return 0;
}
