#include "sample_view.h"

void sample_view_init(sample_view *v, const dk_api *api, uint32_t pool)
{
    v->api = api;
    v->pool = pool;
    v->slot = DK_SAMPLE_POOL_ENTRIES;
    v->token = 0;
    v->peak_count = 0;
    v->win_state = SAMPLE_VIEW_WIN_NONE;
    v->win_count = 0;
    v->last_valid = 0;
    for (unsigned i = 0; i < DK_SAMPLE_POOL_ENTRIES; i++) v->peak_failed[i] = 0;
}

int sample_view_update(sample_view *v, unsigned slot, dk_sample_info *info, int want_peaks)
{
    if (slot >= DK_SAMPLE_POOL_ENTRIES) return DK_SAMPLE_E_CONTRACT;
    int ready = v->api && v->api->sample_pool_info ?
        v->api->sample_pool_info(v->pool, slot, (uint8_t *)info) : DK_SAMPLE_E_CONTRACT;
    if (ready != 0) { v->slot = DK_SAMPLE_POOL_ENTRIES; v->token = 0; v->peak_count = 0; return ready; }
    if (slot != v->slot || info->token != v->token) {
        v->slot = slot; v->token = info->token; v->peak_count = 0;
    }
    if (want_peaks && v->peak_count == 0 && v->peak_failed[slot] != info->token) {
        if (DK_API_HAS(v->api, sample_pool_peaks))
            v->peak_count = v->api->sample_pool_peaks(v->pool, slot, info->token,
                (uint8_t *)v->peaks, sizeof(v->peaks));
        if (v->peak_count <= 0) v->peak_failed[slot] = info->token;
    }
    return 0;
}

const dk_sample_peak *sample_view_peaks(const sample_view *v, unsigned *bins)
{
    if (v->peak_count > 0) { *bins = (unsigned)v->peak_count; return v->peaks; }
    *bins = 0;
    return 0;
}

const dk_sample_peak *sample_view_window(sample_view *v, unsigned slot, uint32_t token, uint32_t first,
    uint32_t span, unsigned columns, unsigned *count)
{
    *count = 0;
    if (!DK_API_HAS(v->api, sample_pool_peaks_window)) return 0;
    int32_t r = v->api->sample_pool_peaks_window(v->pool, slot, token, first, span, columns,
        (uint8_t *)v->window, sizeof(v->window));
    int same = v->win_state != SAMPLE_VIEW_WIN_NONE && v->win_slot == slot && v->win_token == token &&
        v->win_first == first && v->win_span == span && v->win_columns == columns;
    if (r > 0) {
        v->win_slot = slot; v->win_token = token; v->win_first = first; v->win_span = span;
        v->win_columns = columns; v->win_state = SAMPLE_VIEW_WIN_READY; v->win_count = r;
        if (!(v->last_valid && v->last_slot == slot && v->last_token == token && v->last_first == first &&
              v->last_span == span && v->last_columns == columns)) {
            for (int32_t k = 0; k < r && k < (int32_t)DK_SAMPLE_WINDOW_MAX; k++) v->last_window[k] = v->window[k];
            v->last_slot = slot; v->last_token = token; v->last_first = first; v->last_span = span;
            v->last_columns = columns; v->last_valid = 1;
        }
        *count = (unsigned)r;
        return v->window;
    }
    if (r < 0) { v->win_state = SAMPLE_VIEW_WIN_NONE; return 0; }
    if (same && v->win_state == SAMPLE_VIEW_WIN_READY) {
        *count = (unsigned)v->win_count;
        return v->window;
    }
    if (!same) {
        v->win_slot = slot; v->win_token = token; v->win_first = first; v->win_span = span;
        v->win_columns = columns; v->win_state = SAMPLE_VIEW_WIN_PENDING; v->win_count = 0;
    }
    return 0;
}

const dk_sample_peak *sample_view_last_window(const sample_view *v, unsigned slot, uint32_t token,
    uint32_t *first, uint32_t *span, unsigned *columns)
{
    if (!v->last_valid || v->last_slot != slot || v->last_token != token) return 0;
    *first = v->last_first; *span = v->last_span; *columns = v->last_columns;
    return v->last_window;
}

int sample_view_status(const sample_view *v, unsigned slot)
{
    return DK_API_HAS(v->api, sample_pool_status) ?
        v->api->sample_pool_status(v->pool, slot) : DK_SAMPLE_E_CONTRACT;
}
