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

static uint32_t tracks; /* the runtime's handle of the 16-track table; the picker and the store select files with it */

int32_t dkm_init(const dk_api *api)
{
    int32_t handle;
    if (api->version != 1 || api->size < offsetof(dk_api, sample_track_info) + sizeof(api->sample_track_info)
        || !api->sample_tracks)
        return DK_SAMPLE_E_CONTRACT;
    handle = api->sample_tracks(0);
    if (handle <= 0)
        return handle < 0 ? handle : DK_SAMPLE_E_CONTRACT;
    tracks = (uint32_t)handle;
    dk_picker_init(api, tracks); /* the picker is optional: when it cannot start, the SAMPLES... row is absent */
    return 0;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
    dk_picker_fini();
    tracks = 0;
}

/* SETTINGS... of this module in DIGIKIT > MODULES: the sample picker (sdk/src/sample_picker.c). */
static void shot_menu(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    dk_picker_open(m);
}
DK_MENU(shot_menu)
