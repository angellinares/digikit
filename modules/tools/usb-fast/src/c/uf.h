/* digikit/usb-fast: what usb-fast.c and the storage bench (uf-bench.c) share. */
#ifndef UF_H
#define UF_H

#include <dk/core/api.h>

#define UF_TRACE 0 /* per-file time totals in the log at END (hardware hunt); 0 removes them */
#define UF_BENCH                                                                                   \
    0 /* 1 for measurement builds: STATUS body 'B' runs the storage bench, 'R' reads its results (uf-bench.c) */
#define UF_KNOBS                                                                                   \
    0 /* 1 for the staging buffer test: STATUS body 'A' [bytes u32] sets the largest staging buffer of the next BEGIN */

#if UF_TRACE || UF_BENCH
/* a line of the runtime log: `tag` is at most 16 chars (usb-fast.c) */
void uf_log(const char *tag, uint32_t value);
#endif
#if UF_BENCH
/* the bench: STATUS bodies 'B' and 'R' (a reply length, or a status), its job in dkm_init and dkm_fini (uf-bench.c) */
int32_t uf_bench_command(const uint8_t *body, uint32_t len, uint8_t *out);
int32_t uf_bench_init(const dk_api *api);
void uf_bench_fini(const dk_api *api);
#endif

#endif
