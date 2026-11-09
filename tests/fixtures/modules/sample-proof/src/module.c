/* Test-only service: upload first, then 0x3e queues the fixed source path;
   0x3f returns one signed BE32 status word. No file IO in the MIDI callback. */
#include <dk/api.h>
#include <dk/machine.h>
#include <dk/sample.h>

DK_MACHINE(sample_proof, .name = "Sample proof", .short_name = "SMP",
           .render = "_sp_render", .note_on = "_sp_note_on", .desc = "_sp_desc");
/* Explicit frame bounds for the short proof fixture, latched at note-on.
   END=127 selects the file end; defaults select the complete upload. */
DK_PARAM(sample_proof, 0, start, "Start", "START", 0, 127, 0);
DK_PARAM(sample_proof, 1, end, "End", "END", 0, 127, 127);
DK_PARAM(sample_proof, 2, reverse, "Reverse", "REV", 0, 127, 0);
DK_PAGE(sample_proof, "SAMPLE", "Sample proof", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2));
DK_SAMPLE_BANK(0, _sp_bank, _sp_view, 8192);

static const dk_api *runtime;
static uint32_t bank;

static int32_t command(uint32_t cmd, uint32_t seq, const uint8_t *body,
                       uint32_t len, uint8_t *out)
{
    int32_t result;
    (void)seq;
    (void)body;
    if (len != 0)
        return DK_SAMPLE_E_FORMAT;
    if (cmd == 0x3e) {
        const uint8_t *path = (const uint8_t *)(runtime->device == 1
            ? "/proof/test" : "samples/proof/test");
        return runtime->sample_load(bank, path);
    }
    result = runtime->sample_status(bank);
    out[0] = (uint8_t)((uint32_t)result >> 24);
    out[1] = (uint8_t)((uint32_t)result >> 16);
    out[2] = (uint8_t)((uint32_t)result >> 8);
    out[3] = (uint8_t)result;
    return 4;
}

int32_t dkm_init(const dk_api *api)
{
    int32_t handle;
    if (api->version != 1 || api->size < offsetof(dk_api, sample_status) + sizeof(api->sample_status)
        || !api->sample_bank || !api->sample_load || !api->sample_status)
        return DK_SAMPLE_E_CONTRACT;
    runtime = api;
    handle = api->sample_bank(0);
    if (handle <= 0)
        return handle < 0 ? handle : DK_SAMPLE_E_CONTRACT;
    bank = (uint32_t)handle;
    return api->register_cmds(0x3e, 2, command);
}
