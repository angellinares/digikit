/* sample_view.h: the info of the selected pool slot and, on request, a copy of its peak overview. UI task only. */
#ifndef SAMPLE_VIEW_H
#define SAMPLE_VIEW_H
#include <dk/api.h>
#include <dk/sample_track.h>

#ifndef DK_SAMPLE_WINDOW_MAX
#define DK_SAMPLE_WINDOW_MAX 128u
#endif

enum { SAMPLE_VIEW_WIN_NONE, SAMPLE_VIEW_WIN_PENDING, SAMPLE_VIEW_WIN_READY };

typedef struct sample_view {
    const dk_api *api;
    uint32_t pool;                          /* the module's sample-track pool handle */
    uint32_t slot;                          /* slot of the cached peaks; DK_STRACKS = none */
    uint32_t token;                         /* load token of that slot */
    int32_t peak_count;                     /* > 0: peaks holds this many bins */
    uint32_t peak_failed[DK_STRACKS];       /* per slot: the token whose peak fetch failed (not retried) */
    dk_sample_peak peaks[DK_SAMPLE_PEAK_BINS];
    uint32_t win_slot, win_token, win_first, win_span, win_columns; /* key of the detail window */
    int win_state;                          /* SAMPLE_VIEW_WIN_*: the key is requested (pending) or its columns are held (ready) */
    int32_t win_count;                      /* columns held when ready */
    dk_sample_peak window[DK_SAMPLE_WINDOW_MAX];
} sample_view;

void sample_view_init(sample_view *v, const dk_api *api, uint32_t pool); /* api may be 0 (fini): view empty */
int sample_view_update(sample_view *v, unsigned slot, dk_sample_info *info, int want_peaks); /* 0: ready, info filled */
const dk_sample_peak *sample_view_peaks(const sample_view *v, unsigned *bins); /* 0 and *bins = 0 when none */
/* Background min/max of `columns` equal parts of frames [first, first + span); asks the runtime on every call.
   Returns the columns (count set) when they match this request, else 0 (computing, no API, error). */
const dk_sample_peak *sample_view_window(sample_view *v, unsigned slot, uint32_t token, uint32_t first,
    uint32_t span, unsigned columns, unsigned *count);
int sample_view_status(const sample_view *v, unsigned slot); /* sample_pool_status, or DK_SAMPLE_E_CONTRACT */

#endif
