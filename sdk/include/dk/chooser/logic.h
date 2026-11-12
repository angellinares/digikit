/* Pure functions of the chooser and the on-device sample picker: row sort order and the text of pool rows (text and paths: core/text.h, core/path.h). No API calls and no statics, so the
   host tests run them as they are (modules/core/adapters/ui/host/tests/picker.rs). Text buffers are always NUL-terminated. */
#ifndef DK_CHOOSER_LOGIC_H
#define DK_CHOOSER_LOGIC_H
#include <stdint.h>

#define DK_CHOOSER_ROWS 128 /* rows kept per folder */
#define DK_CHOOSER_NAME 48  /* a leaf name, NUL included; a longer name is not shown */
#define DK_CHOOSER_PATH 256 /* path buffer; the public path limit is 255 bytes */
#define DK_CHOOSER_TEXT 23  /* characters of a row (DK_UI_ROW - 1) */

typedef struct dk_chooser_row {
    char name[DK_CHOOSER_NAME];
    uint32_t size;
    uint8_t dir; /* 1: folder */
} dk_chooser_row;

/* 1 when a sorts before b: folders first, then names without regard to case, then the bytes (so the order is total). */
int dk_chooser_row_before(const dk_chooser_row *a, const dk_chooser_row *b);
/* Keeps the first `cap` rows in sort order. rows[idx[0..*n-1]] is sorted. 1 when r is kept, 0 when it sorts after a full table. */
int dk_chooser_row_insert(dk_chooser_row *rows, uint8_t *idx, int *n, int cap,
                          const dk_chooser_row *r);

/* The tail of a pool row for an error code (a DK_SAMPLE_E_*), 14 characters at most; out holds 20 bytes. */
void dk_sample_pool_err_tail(char *out, int code);
/* The INFO text for an error code; out holds 32 bytes. */
void dk_sample_pool_err_info(char *out, int code);

/* A pool row of at most 23 characters: "03 KICK808 S 1.2s". `index` is the pool index as the SMP knob shows it (0 to 15, two digits);
   `current` puts '>' after the number (the entry the SMP value points at). status is sample_pool_status (0 unused, 1 queued, 2 loading,
   3 ready, below 0 an error). name may be "". An unused entry is "03 -". */
void dk_sample_pool_row_text(char *out, int index, int current, int status, const char *name,
                             uint32_t frames, uint32_t rate, uint32_t channels);
#endif
