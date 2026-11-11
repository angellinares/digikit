/* The effect page of test/trace-demo: one effect `gain` with one knob (as test/dyn-fx-gain), and trace marks in dkm_init. */
#include <dk/core/api.h>
#include <dk/fx/fx.h>
#include <dk/core/trace.h>

DK_FX(gain, .process = "_dg_process", .desc = "_dg_desc", .mix = 127);
DK_FX_PARAM(gain, 0, "Gain", "GAIN", DK_PERCENT, 0, 1.27, 0.5);
DK_PAGE(gain, "GAN", "Test Gain", DK_KNOB_AT(0, 0));

int32_t dkm_init(const dk_api *api)
{
    DK_TRACE_INIT(api);
    DK_TRACE_LOG("trace-demo init", 1);
    return 0;
}
