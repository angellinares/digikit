/* DKBS decoder (packages/dkbs, docs/design/dkbs.md): a boot animation of 128 x 64 native bitmaps, at most 300 ticks, in a file of at
   most 65,536 bytes. No firmware calls and no allocation: the caller gives the work buffer. The file bytes must stay unchanged while the
   state is in use. */
#ifndef DKBS_H
#define DKBS_H

#include <stdint.h>

#define DKBS_FILE_MAX 65536u
#define DKBS_TICKS_MAX 300u
#define DKBS_FRAME 1024u

typedef struct {
    const uint8_t *data;
    uint8_t *work; /* DKBS_FRAME bytes: the frame of the current tick */
    uint32_t size, cursor, records, total_ticks;
    uint32_t record, tick, hold, decoded_records;
    int valid;
} Dkbs;

/* Validates the whole file (header, CRC, every record, exact end) before it returns 1. Returns 0 for a file that breaks a rule; `next` then
   returns 0 as well. `work` is DKBS_FRAME bytes that the decoder owns until the state is dropped. */
int dkbs_open(Dkbs *state, const uint8_t *data, uint32_t size, uint8_t *work);
/* The frame of the next tick: a pointer to `work`. A hold (the ticks of one record) returns the same pointer and decodes nothing. Returns 0
   after the last tick, and for a state that is invalid. */
const uint8_t *dkbs_next(Dkbs *state);

#endif
