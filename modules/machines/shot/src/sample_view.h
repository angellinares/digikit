/* sample_view.h: the info of the selected pool slot and, on request, a copy of its peak overview. UI task only. */
#ifndef SAMPLE_VIEW_H
#define SAMPLE_VIEW_H
#include <dk/api.h>
#include <dk/sample_track.h>

typedef struct sample_view {
    const dk_api *api;
    uint32_t pool;                          /* the module's sample-track pool handle */
    uint32_t slot;                          /* slot of the cached peaks; DK_STRACKS = none */
    uint32_t token;                         /* load token of that slot */
    int32_t peak_count;                     /* > 0: peaks holds this many bins */
    uint32_t peak_failed[DK_STRACKS];       /* per slot: the token whose peak fetch failed (not retried) */
    dk_sample_peak peaks[DK_SAMPLE_PEAK_BINS];
} sample_view;

void sample_view_init(sample_view *v, const dk_api *api, uint32_t pool); /* api may be 0 (fini): view empty */
int sample_view_update(sample_view *v, unsigned slot, dk_sample_info *info, int want_peaks); /* 0: ready, info filled */
const dk_sample_peak *sample_view_peaks(const sample_view *v, unsigned *bins); /* 0 and *bins = 0 when none */
int sample_view_status(const sample_view *v, unsigned slot); /* sample_pool_status, or DK_SAMPLE_E_CONTRACT */

#endif
