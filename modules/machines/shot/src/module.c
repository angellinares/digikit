/* The machine declaration of digikit/shot: one stereo machine `shot` that plays a sample of its pool, with five
   parameters shown through two opt-in native pages. patchkit pack reads these records; the device never sees them. The DSP symbols are in src/dsp/shot.s;
   the parameter words are read there in this order: SMP, START, END, PLAY, PITCH.
   The pool (16 entries) is the runtime's per-module table (<dk/sample_track.h>); SMP picks the entry of a note. The module only plays;
   files are loaded into pool slots by the picker (the SMP click list) through the runtime. */
#include <dk/api.h>
#include <dk/machine.h>
#include <dk/params.h>
#include <dk/sample_picker.h>
#include <dk/sample_track.h>
#include <dk/sample_region.h>
#include <dk/widgets.h>
#include <dk/ui.h>

DK_MACHINE(shot, .name = "SHOT", .short_name = "SPK", .render = "_shot_render",
           .note_on = "_shot_note_on", .desc = "_shot_desc", .stereo = 1);
/* SMP: the pool entry that plays, 0 .. 15 (turn: steps through the pool; p-lockable; an empty entry is silent).
   START and END: 0 .. 127 spread over the whole sample (127 is its last frame boundary). PLAY: 0 REV, 1 REV.L, 2 FWD.L, 3 FWD (default); .L loops
   START .. END (the glyph is the stock PLAY knob on the Digitakt II).
   PITCH: 64 is the played note unchanged, each unit is 0.375 semitones, so 0 .. 127 is -24 .. +23.6 semitones. */
DK_PARAM(shot, 0, shot_smp, "Sample", "SMP", 0, 15, 0);
DK_PARAM_LATCHED(shot, 0);
DK_PARAM(shot, 1, shot_start, "Start", "START", 0, 127, 0);
DK_PARAM(shot, 2, shot_end, "End", "END", 0, 127, 127);
DK_PARAM(shot, 3, shot_play, "Play", "PLAY", 0, 3, 3);
DK_PARAM_GLYPH(shot, 3, DK_GLYPH_PLAY);
DK_PARAM(shot, 4, shot_pitch, "Pitch", "TUNE", 0, 127, 64);
DK_PAGE(shot, "SPK", "SHOT", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2), DK_KNOB(3), DK_KNOB(4));
DK_NATIVE_PAGE_V2_FLAGS(shot, 0, DK_PAGE_STOCK_OVERLAY, "SHOT", shot_controls_draw, shot_range_event,
                 DK_PAGE_PARAM(4, 0), DK_PAGE_PARAM(3, 1), DK_PAGE_PARAM(0, 3), DK_PAGE_PARAM(1, 4), DK_PAGE_PARAM(2, 7));
DK_NATIVE_PAGE_V2_FLAGS(shot, 1, DK_PAGE_WIDE_CANVAS, "SHOT WAVE", shot_range_draw, shot_range_event,
                 DK_PAGE_PARAM(1, 4), DK_PAGE_LOCAL(0, 6), DK_PAGE_PARAM(2, 7));
DK_SAMPLE_TRACKS(0, _shot_rec, _shot_ctl);

/* UI-only state. The existing START/END words continue through native editing;
   zoom consumes its own turn and never writes a parameter or sample epoch. */
static int32_t page_zoom[16]; /* Q8 ratio offset from 1x: 0..768, 1/64x per detent */
static const dk_api *g_api;
static uint32_t pool;
static dk_sample_peak page_peaks[DK_SAMPLE_PEAK_BINS];
static uint32_t peak_slot = 16, peak_token;
static uint32_t peak_failed[DK_STRACKS];
static int32_t peak_count;
static char peak_name[256];

static void page_number(const dk_page_canvas_v1 *canvas, int center, int y, unsigned value)
{
    char text[12];
    char *at = text + sizeof(text) - 1;
    *at = 0;
    do { *--at = (char)('0' + value % 10); value /= 10; } while (value);
    canvas->text(center - (int)(text + sizeof(text) - 1 - at) * 6 / 2, y, at);
}

/* One current sample view: basename is useful on page zero without requesting
   an overview. Peak copies are requested only by the waveform page. */
static int sample_view(unsigned slot, dk_sample_info *info, int waveform)
{
    if (slot >= DK_STRACKS) return DK_SAMPLE_E_CONTRACT;
    int ready = g_api && g_api->sample_pool_info ?
        g_api->sample_pool_info(pool, slot, (uint8_t *)info) : DK_SAMPLE_E_CONTRACT;
    if (ready != 0) { peak_slot = 16; peak_token = 0; peak_count = 0; return ready; }
    if (slot != peak_slot || info->token != peak_token) {
        peak_slot = slot; peak_token = info->token; peak_count = 0;
        peak_name[0] = 0;
        if (g_api->size >= offsetof(dk_api, sample_pool_path) + sizeof(g_api->sample_pool_path) && g_api->sample_pool_path)
            g_api->sample_pool_path(pool, slot, (uint8_t *)peak_name, sizeof(peak_name));
    }
    if (waveform && peak_count == 0 && peak_failed[slot] != info->token) {
        if (g_api->size >= offsetof(dk_api, sample_pool_peaks) + sizeof(g_api->sample_pool_peaks) && g_api->sample_pool_peaks)
            peak_count = g_api->sample_pool_peaks(pool, slot, info->token, (uint8_t *)page_peaks, sizeof(page_peaks));
        if (peak_count <= 0) peak_failed[slot] = info->token;
    }
    return 0;
}

static unsigned unit(int32_t raw)
{
    unsigned value = raw <= 0 ? 0 : (unsigned)raw >> 8;
    return value > 127 ? 127 : value;
}

static void region_values(const dk_page_frame_v1 *f)
{
    const dk_page_canvas_v1 *c = &f->canvas;
    c->text(8, 43, "START"); c->text(89, 43, "END");
    page_number(c, 19, 51, unit(f->word_values[1]));
    page_number(c, 97, 51, unit(f->word_values[2]));
}

void shot_controls_draw(const dk_page_frame_v1 *f)
{
    if (f->version != 1 || f->size < sizeof(*f) || f->track >= 16 || f->word_count < 5) return;
    const dk_page_canvas_v1 *c = &f->canvas;
    unsigned slot = unit(f->word_values[0]);
    dk_sample_info info;
    int ready = sample_view(slot, &info, 0);
    /* Only top A/B/D glyph rectangles are replaced. Native labels, E/H
       encoders and the separate physical LEV fader stay untouched. */
    c->fill_rect(0, 0, 22, 19, 0);
    dk_page_tune(c, 0, 0, ((int)unit(f->word_values[4])-64)*3);
    c->fill_rect(24, 0, 47, 19, 0);
    dk_sample_play_glyph(c, 29, 7, unit(f->word_values[3]));
    c->fill_rect(76, 0, 98, 19, 0);
    int state = ready == 0 ? DK_SAMPLE_READY :
        g_api && g_api->sample_pool_status ? g_api->sample_pool_status(pool, slot) : DK_SAMPLE_E_CONTRACT;
    unsigned visual = ready == 0 ? DK_WIDGET_SAMPLE_READY :
        state == DK_SAMPLE_UNUSED ? DK_WIDGET_SAMPLE_EMPTY :
        state < 0 ? DK_WIDGET_SAMPLE_MISSING : DK_WIDGET_SAMPLE_LOADING;
    dk_page_sample(c, 76, 0, slot, visual);
}

static void zoom_number(const dk_page_canvas_v1 *c, int x, int y, unsigned ratio)
{
    unsigned value = (ratio * 100u + 128u) / 256u;
    char text[5] = {(char)('0' + value / 100), '.', (char)('0' + value / 10 % 10), (char)('0' + value % 10), 0};
    c->text(x, y, text);
}

void shot_range_draw(const dk_page_frame_v1 *f)
{
    if (f->version != 1 || f->size < sizeof(*f) || f->track >= 16 || f->word_count < 5) return;
    unsigned slot = unit(f->word_values[0]);
    dk_sample_info info;
    int ready = sample_view(slot, &info, 1);
    unsigned ratio = DK_SAMPLE_ZOOM_ONE + (unsigned)page_zoom[f->track];
    dk_sample_region_rect plot = {0, 6, 94, 36};
    dk_sample_region_rect_q8(&f->canvas, ready == 0 && peak_count > 0 ? page_peaks : 0,
        ready == 0 && peak_count > 0 ? (unsigned)peak_count : 0, ready == 0 ? info.frames : 0,
        unit(f->word_values[1]), unit(f->word_values[2]), ratio, plot);
    region_values(f);
    f->canvas.text(58, 43, "ZOOM");
    zoom_number(&f->canvas, 59, 51, ratio);
}

void shot_range_event(const dk_page_event_v1 *event)
{
    if (event->version != 1 || event->size < sizeof(*event) || event->track >= 16 || event->control != 0)
        return;
    int delta = event->delta;
    if (delta < -192) delta = -192;
    if (delta > 192) delta = 192;
    int next = page_zoom[event->track] + delta * 4;
    page_zoom[event->track] = next < 0 ? 0 : next > 768 ? 768 : next;
}

static uint32_t click_track; /* the track of the page the SMP click came from: where the pick sets SMP */

int32_t dkm_init(const dk_api *api)
{
    int32_t handle;
    if (api->version != 1 || api->size < offsetof(dk_api, sample_pool_info) + sizeof(api->sample_pool_info) || !api->sample_tracks)
        return DK_SAMPLE_E_CONTRACT;
    handle = api->sample_tracks(0);
    if (handle <= 0)
        return handle < 0 ? handle : DK_SAMPLE_E_CONTRACT;
    pool = (uint32_t)handle;
    g_api = api;
    dk_picker_init(api, pool); /* the picker is optional: when it cannot start, a click on SMP does nothing */
    return 0;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
    dk_picker_fini();
    pool = 0;
    g_api = 0;
    peak_slot = 16; peak_token = 0; peak_count = 0;
}

/* The pick: SMP of the clicked track becomes the pool index (raw = unit * 256). The lists have closed, the page is on top. A refusal shows on
   the page, not in a dialog. */
static void smp_pick(void *ctx, int index)
{
    int32_t rc;
    (void)ctx;
    if (!g_api || g_api->size < offsetof(dk_api, param_set) + sizeof g_api->param_set || !g_api->param_set)
        return;
    rc = g_api->param_set(click_track, 0, index << 8);
    if (rc == DK_PARAM_E_HELD)
        dk_ui_toast("RELEASE TRIG TO SET SMP");
    else if (rc < 0)
        dk_ui_toast("SMP NOT SET");
}

/* Click on SMP at D (release without a turn on the custom SHOT page): the runtime's page hook calls this in the UI task with the track of the page.
   It opens the slot list over the page (sdk/src/sample_picker.c) with the current SMP value marked and the cursor on it. YES on a slot opens
   the file browser for it; YES on a file loads it into that slot, sets SMP to the slot and returns to the page in one step
   (docs/design/ui-rules.md); NO goes back one level. */
DK_PARAM_ACTION(shot, 0, shot_smp_click);
void shot_smp_click(uint32_t track)
{
    int32_t raw = -1;
    click_track = track;
    if (g_api && g_api->size >= offsetof(dk_api, param_get) + sizeof g_api->param_get && g_api->param_get)
        raw = g_api->param_get(track, 0);
    dk_picker_open_pool(DK_PARAM_IS_ERROR(raw) || raw < 0 ? -1 : (int)(raw >> 8), smp_pick, 0);
}
