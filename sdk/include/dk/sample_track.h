/* Per-module stereo sample pool (16 entries). API contract 1.5, sample_pool_path 1.6 (docs/design/sample-api-1.5.md).
   Additive to <dk/sample.h> (1.3 bank, frozen, mono, 8192 frames, float): a module
   uses the bank family or the track family, never both (the second is refused with
   DK_SAMPLE_E_CONTRACT). A pool entry holds one PCM16 sample, mono or stereo, in
   planar form; the runtime owns the memory and the module never sees a device
   address, a slot or a pool. */
#ifndef DK_SAMPLE_TRACK_H
#define DK_SAMPLE_TRACK_H
#include <dk/sample.h>
#include <dk/gen/desc.h>

#define DK_STRACKS 16u

/* States are DK_SAMPLE_UNUSED/QUEUED/LOADING/READY (0..3); errors are negative.
   Reused errors: NOT_FOUND -1, READ -3, FORMAT -5, TOO_BIG -13, CONTRACT -48.
   In this family TOO_BIG means "does not fit the live pool". */
#define DK_SAMPLE_E_LOST (-15)    /* the backing was lost (DT2 stock reset) and the reload failed */
#define DK_SAMPLE_E_POOL (-16)    /* the pool is full */
#define DK_SAMPLE_E_CHANGED (-17) /* the file differs from the selection snapshot */
/* The design draft numbers E_CHANGED -14; -14 is E_TABLE_FULL, so it is -17. */
#define DK_SAMPLE_E_SEL_PATH (-40) /* the path is empty, too long or bad; sample_pool_path: buf too small */

/* One record per pool entry in the module's DSP data, eight normal words, the runtime
   writes and the module reads. `ready` is published last. `token` is nonzero,
   changes on every load and never wraps or repeats. */
typedef struct dk_strack {
    uint32_t ready;    /* 0 or 1 */
    uint32_t token;
    uint32_t frames;
    uint32_t rate;     /* Hz */
    uint32_t channels; /* 1 or 2 */
    uint32_t fmt;      /* DK_STRACK_FMT_PCM16 */
    uint32_t base_l;   /* DSP normal-word address of plane L; DDR PCM uses the NW32 alias 0x10000000 + (phys - 0x80000000) / 4 (finding 99) */
    uint32_t base_r;   /* plane R, same form; == base_l when channels == 1 */
} dk_strack;
_Static_assert(sizeof(dk_strack) == 32, "dk_strack layout");
#define DK_STRACK_WORDS 8u
#define DK_STRACK_FMT_PCM16 1u /* packed PCM16 (finding 87); float is not offered */

/* The per-entry control set the runtime and the bank macros of sdk/dsp/sample_bank.inc
   share: four words per entry. Offsets in words. */
#define DK_STRACK_CTL_CLOSED 0u
#define DK_STRACK_CTL_COUNT 1u
#define DK_STRACK_CTL_FAULT 2u
#define DK_STRACK_CTL_WORDS 4u

/* Result of sample_pool_info, 16 bytes, READY entries only. */
typedef struct dk_sample_info {
    uint32_t frames, rate, channels, token;
} dk_sample_info;
_Static_assert(sizeof(dk_sample_info) == 16, "dk_sample_info layout");

/* Declares the table: RECORDS names 16 zeroed dk_strack in the module's DSP data
   (DK_STRACKS * 8 words); CONTROL names 16 control sets of 4 zeroed words.
   The runtime fills the records; the module never writes them. */
#define DK_SAMPLE_TRACKS(LOCAL_ID, RECORDS, CONTROL)                                \
    static const dk_desc_sample_tracks DK_CAT(dk_sample_tracks_, __LINE__) DK_DESC = { \
        .kind = DK_DESC_SAMPLE_TRACKS, .size = sizeof(dk_desc_sample_tracks),      \
        .local_id = (LOCAL_ID), .records = #RECORDS, .control = #CONTROL}

/* Api entries (resolved in init like sample_bank; see <dk/gen/api.h>). The module owns one pool of DK_STRACKS (16) entries; the entry
   index is the record index the DSP reads (a machine picks it with a parameter, for example SHOT's SMP):
     int32_t sample_tracks(uint32_t local_id);                 handle > 0, or error
     int32_t sample_pool_add(h, const uint8_t *path);          the pool index 0..15, or a negative error
     int32_t sample_pool_set(h, index, const uint8_t *path);   (api 1.6) 0 or error: load into that index, replacing what was there
     int32_t sample_pool_clear(h, index);                      0 or error; index >= 16: E_CONTRACT
     int32_t sample_pool_status(h, index);                     sticky errors until the entry is added again or cleared
     int32_t sample_pool_info(h, index, uint8_t *out);         out is a dk_sample_info
     int32_t sample_pool_path(h, index, uint8_t *buf, uint32_t cap);   (api 1.6) length, or error
   set loads the file into the given index (a filled entry is replaced in place, the old file is released; a file another entry holds is
   shared); index >= 16: E_CONTRACT. add returns the first UNUSED index. A file that is already in the pool (the same path) returns its index and loads nothing; if that
   entry has a sticky error, the add selects it again. Indexes are stable: clear leaves a hole and never moves another entry, so a value
   saved in a sound or a p-lock keeps its meaning. A full pool returns DK_SAMPLE_E_POOL. An entry that is not READY (UNUSED, QUEUED,
   LOADING, an error) is silent in the DSP: note_on is denied and the voice ends. The pool is kept per project; a project change
   reloads the list. Calls are legal from init and service/UI callbacks. A runtime image without a sample backing returns
   DK_SAMPLE_E_CONTRACT from sample_pool_add.

   Paths (api 1.6). sample_pool_add takes the root-relative path of a DK_FILES_SAMPLES listing
   row, as <dk/files.h> gives it ("KICKS/K1", no leading slash); the runtime adds the device's
   root. A DT2 path that already starts with '/' (the 1.5 form) is accepted as it is.
   sample_pool_path returns the path of an entry in the root-relative form, in
   every state but UNUSED (E_NOT_FOUND): it is what a UI shows after a project change
   reloaded the list. It copies the NUL-terminated path into buf and returns its length
   without the NUL; if length + 1 > cap it returns DK_SAMPLE_E_SEL_PATH and leaves buf alone.
   A module that calls it checks that api->size covers sample_pool_path (API 1.6) first. */
#endif
