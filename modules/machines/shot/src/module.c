/* The machine declaration of digikit/shot: one stereo machine `shot` that plays a sample of its pool, with five
   parameters on one page. patchkit pack reads these records; the device never sees them. The DSP symbols are in src/dsp/shot.s;
   the parameter words are read there in this order: SMP, START, END, REVERSE, PITCH.
   The pool (16 entries) is the runtime's per-module table (<dk/sample_track.h>); SMP picks the entry of a note. The module only plays;
   files are loaded into pool slots by the picker (the SMP click list) through the runtime. */
#include <dk/api.h>
#include <dk/machine.h>
#include <dk/params.h>
#include <dk/sample_picker.h>
#include <dk/sample_track.h>
#include <dk/ui.h>

DK_MACHINE(shot, .name = "SHOT", .short_name = "SPK", .render = "_shot_render",
           .note_on = "_shot_note_on", .desc = "_shot_desc", .stereo = 1);
/* SMP: the pool entry that plays, 0 .. 15 (turn: steps through the pool; p-lockable; an empty entry is silent).
   START and END: 0 .. 127 spread over the whole sample (127 is its last frame boundary). REVERSE: 64 and above plays backwards.
   PITCH: 64 is the played note unchanged, each unit is 0.375 semitones, so 0 .. 127 is -24 .. +23.6 semitones. */
DK_PARAM(shot, 0, shot_smp, "Sample", "SMP", 0, 15, 0);
DK_PARAM(shot, 1, shot_start, "Start", "START", 0, 127, 0);
DK_PARAM(shot, 2, shot_end, "End", "END", 0, 127, 127);
DK_PARAM(shot, 3, shot_reverse, "Reverse", "REV", 0, 127, 0);
DK_PARAM(shot, 4, shot_pitch, "Pitch", "PITCH", 0, 127, 64);
DK_PAGE(shot, "SPK", "SHOT", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2), DK_KNOB(3), DK_KNOB(4));
DK_SAMPLE_TRACKS(0, _shot_rec, _shot_ctl);

static const dk_api *g_api;
static uint32_t pool; /* the runtime's handle of the 16-entry pool table; the picker loads and clears files with it */
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

/* Click on SMP (release without a turn on the SHOT page): the runtime's page hook calls this in the UI task with the track of the page.
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
