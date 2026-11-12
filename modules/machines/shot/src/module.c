/* The machine declaration of digikit/shot: one stereo machine `shot` that plays a sample of its pool, with five
   parameters shown through two opt-in native pages. patchkit pack reads these records; the device never sees them.
   The DSP symbols are in src/dsp/shot.s; the parameter words (src/shot_params.h) are shared with it.
   src/pages.c draws the two pages; src/sample_view.c reads the pool slot they show.
   The pool (16 entries) is the runtime's per-module table (<dk/sample/pool.h>); SMP picks the entry of a note. The module only plays;
   files are loaded into pool slots by the picker (the SMP click list) through the runtime. */
#include <dk/sample/picker.h>
#include "shot.h"

DK_MACHINE(shot, .name = "SHOT", .short_name = "SHT", .render = "_shot_render",
           .note_on = "_shot_note_on", .desc = "_shot_desc", .stereo = 1);
/* SMP: the pool entry that plays, 0 .. 15 (turn: steps through the pool; p-lockable; an empty entry is silent).
   START and END: 0 .. 127 spread over the whole sample (127 is its last frame boundary). PLAY: 0 REV, 1 REV.L, 2 FWD.L, 3 FWD (default); .L loops
   START .. END (the glyph is the stock PLAY knob on the Digitakt II).
   PITCH: 64 is the played note unchanged, each unit is 0.375 semitones, so 0 .. 127 is -24 .. +23.6 semitones. */
DK_PARAM(shot, SHOT_P_SMP, shot_smp, "Sample", "SMP", 0, 15, 0);
DK_PARAM_LATCHED(shot, SHOT_P_SMP);
DK_PARAM(shot, SHOT_P_START, shot_start, "Start", "START", 0, 127, 0);
DK_PARAM(shot, SHOT_P_END, shot_end, "End", "END", 0, 127, 127);
DK_PARAM(shot, SHOT_P_PLAY, shot_play, "Play", "PLAY", 0, 3, 3);
DK_PARAM_GLYPH(shot, SHOT_P_PLAY, DK_GLYPH_PLAY);
DK_PARAM(shot, SHOT_P_PITCH, shot_pitch, "Pitch", "TUNE", 0, 127, 64);
DK_PAGE(shot, "SHT", "SHOT", DK_KNOB(SHOT_P_SMP), DK_KNOB(SHOT_P_START), DK_KNOB(SHOT_P_END),
        DK_KNOB(SHOT_P_PLAY), DK_KNOB(SHOT_P_PITCH));
DK_NATIVE_PAGE_V2_FLAGS(shot, 0, DK_PAGE_STOCK_OVERLAY, "SHOT", shot_controls_draw,
                        shot_range_event, DK_PAGE_PARAM(SHOT_P_PITCH, 0),
                        DK_PAGE_PARAM(SHOT_P_PLAY, 1), DK_PAGE_PARAM(SHOT_P_SMP, 3),
                        DK_PAGE_PARAM(SHOT_P_START, 4), DK_PAGE_PARAM(SHOT_P_END, 7));
DK_NATIVE_PAGE_V2_FLAGS(shot, 1, DK_PAGE_WIDE_CANVAS, "SHOT WAVE", shot_range_draw,
                        shot_range_event, DK_PAGE_PARAM(SHOT_P_START, 4), DK_PAGE_LOCAL(0, 6),
                        DK_PAGE_PARAM(SHOT_P_END, 7));
DK_SAMPLE_TRACKS(0, _shot_rec, _shot_ctl);

sample_view shot_samples;
static const dk_api *g_api;
static uint32_t
    click_track; /* the track of the page the SMP click came from: where the pick sets SMP */

int32_t dkm_init(const dk_api *api)
{
    int32_t handle;
    if (api->version != 1 || !DK_API_HAS(api, sample_pool_info) || !api->sample_tracks)
        return DK_SAMPLE_E_CONTRACT;
    handle = api->sample_tracks(0);
    if (handle <= 0)
        return handle < 0 ? handle : DK_SAMPLE_E_CONTRACT;
    sample_view_init(&shot_samples, api, (uint32_t)handle);
    g_api = api;
    dk_sample_picker_init(
        api,
        (uint32_t)
            handle); /* the picker is optional: when it cannot start, a click on SMP does nothing */
    return 0;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
    dk_sample_picker_fini();
    g_api = 0;
    sample_view_init(&shot_samples, 0, 0);
}

/* The pick: SMP of the clicked track becomes the pool index (raw = unit * 256). The lists have closed, the page is on top. A refusal shows on
   the page, not in a dialog. */
static void smp_pick(void *ctx, int index)
{
    int32_t rc;
    (void)ctx;
    if (!DK_API_HAS(g_api, param_set))
        return;
    rc = g_api->param_set(click_track, SHOT_P_SMP, index << 8);
    if (rc == DK_PARAM_E_HELD)
        dk_ui_toast("RELEASE TRIG TO SET SMP");
    else if (rc < 0)
        dk_ui_toast("SMP NOT SET");
}

/* Click on SMP at D (release without a turn on the custom SHOT page): the runtime's page hook calls this in the UI task with the track of the page.
   It opens the slot list over the page (sdk/src/sample/picker.c) with the current SMP value marked and the cursor on it. YES on a slot opens
   the file browser for it; YES on a file loads it into that slot, sets SMP to the slot and returns to the page in one step
   (docs/design/ui-rules.md); NO goes back one level. */
DK_PARAM_ACTION(shot, SHOT_P_SMP, shot_smp_click);
void shot_smp_click(uint32_t track)
{
    int32_t raw = -1;
    click_track = track;
    if (DK_API_HAS(g_api, param_get))
        raw = g_api->param_get(track, SHOT_P_SMP);
    dk_sample_picker_open_pool(DK_PARAM_IS_ERROR(raw) || raw < 0 ? -1 : (int)(raw >> 8), smp_pick,
                               0);
}
