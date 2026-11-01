/* The sample backing plug-in point, version 1. NOT an SDK contract: content modules never see it.
   The runtime (device-neutral) calls the optional symbols dkpool_* that a device adapter
   exports (modules/core/adapters/sample-pool); the adapter calls dkrt_pool_done and
   dkrt_pool_lost. Without the adapter the symbols resolve to 0 and sample_pool_add returns
   DK_SAMPLE_E_CONTRACT. docs/design/sample-api-1.5.md, section 2.2.
   All calls run in the runtime's task context. `id` and `path` belong to the runtime;
   the adapter does not keep them past the call. Additive growth only: new symbols are
   optional, existing signatures never change.

   Versioning. DKPOOL_VERSION counts breaking changes of the required symbols (dkpool_version,
   dkpool_load, dkpool_release, dkpool_check, dkpool_free_frames) and of the meaning of their
   arguments; it stays 1. An optional symbol (dkpool_tick, dkpool_cancel) is added without a
   version change: the runtime tests its address for 0 and does without it. The adapter lists
   the optional symbols it implements in its manifest; the runtime lists them in its
   `optional` imports. */
#ifndef DKRT_POOL_H
#define DKRT_POOL_H
#include <stdint.h>

#define DKPOOL_VERSION 1u
/* dkpool_version() returns DKPOOL_VERSION; the runtime refuses a different value. */
uint32_t dkpool_version(void);

/* Errors are the runtime's load codes (sdk/sdk.toml [load.status]). */
#define DKPOOL_E_NOT_FOUND (-1)
#define DKPOOL_E_READ (-3)
#define DKPOOL_E_FORMAT (-5)
#define DKPOOL_E_LOST (-15)
#define DKPOOL_E_POOL (-16)
#define DKPOOL_E_CHANGED (-17) /* runtime only: the file differs from the selection snapshot */
#define DKPOOL_E_CONTRACT (-48) /* the caller broke the contract (no free request, token counter exhausted) */
#define DKPOOL_DONE 0   /* dkpool_load finished, `out` is filled */
#define DKPOOL_QUEUED 1 /* dkrt_pool_done(req, result) follows later */

/* File identity snapshot taken by the runtime at select. Two selections with equal
   identity share one backing handle. */
#define DKPOOL_ID_HASH_VALID 1u /* hash_kind/hash from the listing row are valid */
typedef struct dk_file_id {
    uint32_t root;      /* DK_FILES_SAMPLES or DK_FILES_STORE */
    uint32_t size;      /* source physical bytes */
    uint32_t mtime;     /* 0 when unknown; used when no hash is valid */
    uint32_t flags;     /* DKPOOL_ID_* */
    uint32_t hash_kind; /* DK_FILE_HASH_* */
    uint8_t hash[32];
} dk_file_id;

/* What the runtime stores per dedup entry. */
typedef struct dkpool_handle {
    uint32_t id;      /* adapter-private (DT2: slot; DN2: block index) */
    uint32_t token;   /* nonzero, unique per load, never reused */
    uint32_t base_l;  /* DSP normal-word address of plane L, as dk_strack.base_l: NW32 alias of DDR, 0x10000000 + (phys - 0x80000000) / 4 */
    uint32_t base_r;  /* plane R; == base_l for mono */
    uint32_t frames;
    uint32_t rate;    /* Hz */
    uint32_t channels;/* 1 or 2 */
} dkpool_handle;

/* Alloc, read, convert, make DSP-visible. `req` is chosen by the runtime and returned by
   dkrt_pool_done; `out` stays runtime-owned and valid until the load is terminal and the
   adapter fills it (all seven words, rate and channels included) before it reports DONE or calls dkrt_pool_done. Returns DKPOOL_DONE, DKPOOL_QUEUED or a negative
   error (E_POOL pool full, E_READ, E_FORMAT). Reusing a slot of the same file is allowed. */
int32_t dkpool_load(uint32_t req, const dk_file_id *id, const uint8_t *path, dkpool_handle *out);
/* PCM of other handles never moves. */
void dkpool_release(const dkpool_handle *h);
/* A few loads. 0 valid, DKPOOL_E_LOST when the backing was lost externally. */
int32_t dkpool_check(const dkpool_handle *h);
/* Free space counted in mono planes: a stereo sample of N frames needs 2N. */
uint32_t dkpool_free_frames(void);
/* Optional symbol. The runtime calls it once per UI-loop pass (it owns the hook). The adapter delivers finished loads with
   dkrt_pool_done there, and detects a stock reset and reports it with dkrt_pool_lost. Never call the runtime from another task. */
void dkpool_tick(void);
/* Optional symbol (added after version 1, no version change). The runtime no longer waits for the load `req`
   of dkpool_load that returned DKPOOL_QUEUED: a select superseded it, or its last track was unbound, while it was
   still loading. The adapter may stop early (drop a job that has not started, skip the rest of the pages). It must
   still end the request the usual way, once: dkrt_pool_done(req, result) from dkpool_tick, with DKPOOL_E_LOST when
   it stopped early. The runtime frees the entry after that (and calls dkpool_release if the load finished OK, so
   the adapter may also report the real result and let the release follow). A load that is already finished is not
   cancelled. An adapter without the symbol behaves as before: the load runs to its end and its result is released.
   Runs in the UI-loop task, like every dkpool_* call. */
void dkpool_cancel(uint32_t req);

/* Adapter to runtime. result: 0, DKPOOL_E_POOL, DKPOOL_E_READ or DKPOOL_E_FORMAT. */
void dkrt_pool_done(uint32_t req, int32_t result);
/* "Backing lost": all handles are invalid (reset). */
void dkrt_pool_lost(void);
/* Project adapter to runtime: the current project slot changed. The runtime never reads the byte. */
void dkrt_project_changed(uint32_t slot);
#endif
