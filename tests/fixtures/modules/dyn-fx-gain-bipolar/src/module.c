/* The effect of test/dyn-fx-gain-bipolar: one effect `gain` with one signed knob, declared with builder calls in init.
   Tab GBP. BIAS is p0, -64 .. +63, default 0: the routine scales the block by 1 + p0 / 64.
   The MIX knob starts at 127 (wet). The effect starts on the master and can be switched to a send. */
#include <dk/core/module.h>
#include <dk/gen/page_consts.h>
#include "module_ids.h"

static int32_t init(dk_module_ctx *ctx)
{
    dk_fx *fx = dk_module_ctx_add_fx(ctx, DK_FX_ID_GAIN, "Test Gain Bipolar", "GBP");
    dk_fx_routes(fx, DK_ROUTE_MASTER | DK_ROUTE_SEND, DK_ROUTE_MASTER);
    dk_page *p = dk_fx_add_page(fx, "Test Gain Bipolar", "GBP");
    dk_page_add_fx_param(p, "Bias", "BIAS", DK_BIPOLAR, -64.0f, 63.0f, 0.0f);
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
