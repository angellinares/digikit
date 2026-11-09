# DKBS: the boot screen file format (version 1, provisional)

A DKBS file is a 128 x 64, 1-bit animation. Code: `packages/dkbs/src/index.ts` (encoder and decoder) and the boot screen module's C decoder, which reads the same bytes.

## Limits

- 1 to 300 records, 1 to 300 ticks in total, one tick is 1/30 s (30 ticks per second).
- The file is at most 65,536 bytes.

## Header (16 bytes, big endian, `>4sHHII`)

| Field | Meaning |
| --- | --- |
| magic | `DKBS` |
| version | 1 |
| record count | 1..300 |
| total ticks | 1..300, equals the sum of the hold ticks |
| CRC32 | of the body (all bytes after the header) |

## Record (8 bytes, `>HHB3x`, then the payload)

| Field | Meaning |
| --- | --- |
| hold ticks | 1..300, how long the frame stays |
| payload length | in bytes, not 0; RAW must be 1024 |
| codec | 0 RAW, 1 XOR-RUN |
| reserved | 3 bytes, zero |

## Frame

1024 bytes in the native bitmap layout: column-major, the byte for pixel (x, y), with y = 0 at the top of the panel, is at `8*x + (63-y)//8`, the bit is `7 - (63-y)%8`. So pixel (0, 0) is byte 7, mask 0x01, and pixel (0, 63) is byte 0, mask 0x80. The vertical order is measured on a real Digitakt II: frames drawn into the intro appear flipped vertically on the panel unless they use this order; x is not flipped.

## Codecs

- **RAW (0):** the payload is the frame.
- **XOR-RUN (1):** the payload expands to 1024 bytes, which are XORed with the previous decoded frame (all zero before the first record). Control byte 0..127: `ctrl+1` literal bytes follow. Control byte 128..255: one byte follows and repeats `(ctrl & 127) + 1` times. The expansion must be exactly 1024 bytes.

The decoder rejects a bad magic or version, a CRC mismatch, a hold sum that differs from the total, a nonzero reserved byte, a truncated record and trailing bytes.
