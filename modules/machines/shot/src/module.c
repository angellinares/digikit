/* The machine declaration of digikit/shot: one stereo machine `shot` that plays a sample of its pool, with five
   parameters on one page. patchkit pack reads these records; the device never sees them. The DSP symbols are in src/dsp/shot.s;
   the parameter words are read there in this order: SMP, START, END, REVERSE, PITCH.
   The pool (16 entries) is the runtime's per-module table (<dk/sample_track.h>); SMP picks the entry of a note. The module only plays;
   files are added to the pool by the picker (the SMP click list) through the runtime. */
#include <dk/api.h>
#include <dk/machine.h>
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

static uint32_t pool; /* the runtime's handle of the 16-entry pool table; the picker adds and clears files with it */

int32_t dkm_init(const dk_api *api)
{
    int32_t handle;
    if (api->version != 1 || api->size < offsetof(dk_api, sample_pool_info) + sizeof(api->sample_pool_info) || !api->sample_tracks)
        return DK_SAMPLE_E_CONTRACT;
    handle = api->sample_tracks(0);
    if (handle <= 0)
        return handle < 0 ? handle : DK_SAMPLE_E_CONTRACT;
    pool = (uint32_t)handle;
    dk_picker_init(api, pool); /* the picker is optional: when it cannot start, a click on SMP does nothing */
    return 0;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
    dk_picker_fini();
    pool = 0;
}

/* The click on SMP (release without a turn on the SHOT page). The page hook (core/adapters/ui, the SMP DK_PARAM_ACTION) calls
     shot_smp_action(track, value, set)
   in the UI task: track is the track of the page (0 to 15), value the SMP value of that track now (0 to 15), and set(track, index) is the
   hook's own function that writes SMP of `track` (the value shown on the knob and the one a p-lock or the next note reads). This opens the
   pool list over the page (sdk/src/sample_picker.c). The picker calls set when a file was added (index of its entry) or YES chose an
   entry. NO in the list returns to the page without a call. The module cannot write a parameter itself: set is what the hook provides. */
typedef void shot_smp_set_fn(uint32_t track, uint32_t index);
static shot_smp_set_fn *smp_set;
static uint32_t smp_track;

static void shot_smp_pick(void *ctx, int index)
{
    (void)ctx;
    if (smp_set && index >= 0)
        smp_set(smp_track, (uint32_t)index);
}

void shot_smp_action(uint32_t track, uint32_t value, shot_smp_set_fn *set)
{
    smp_track = track;
    smp_set = set;
    dk_picker_open_pool(value < DK_STRACKS ? (int)value : -1, shot_smp_pick, 0);
}
