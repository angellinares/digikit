/* The effect of __ID__, declared by builder calls in init: one effect with one GAIN knob and the framework's MIX knob. Saved kits keep the effect by
   module id + effect id: never rename either. */
#include <dk/core/module.h>
#include <dk/gen/page_consts.h>
#include "module_ids.h"
/* Trace marks for C code: see sdk/README.md, "Tracing" (`<dk/core/trace.h>`, `DK_TRACE_LOG`). The DSP side uses `DK_STEP` of dk_trace.inc. */

static int32_t init(dk_module_ctx *ctx)
{
    dk_fx *fx = dk_module_ctx_add_fx(ctx, DK_FX_ID___UPPER__, "__TITLE__", "__SHORT__");
    dk_page *p = dk_fx_add_page(fx, "__TITLE__", "__TAB__");
    dk_page_add_fx_param(p, "Gain", "GAIN", DK_PERCENT, 0.0f, 1.27f, 0.5f);
    /* A signed knob shows -64 .. +63: dk_page_add_fx_param(p, "Pan", "PAN", DK_BIPOLAR, -64.0f, 63.0f, 0.0f); (min, max and def are in the parameter's unit) */
    /* The routes the effect may run on (DK_ROUTE_MASTER, DK_ROUTE_SEND or both) and the default one; the MIX knob's default (0 to 127). */
    dk_fx_routes(fx, DK_ROUTE_MASTER | DK_ROUTE_SEND, DK_ROUTE_MASTER);
    dk_fx_mix(fx, 127);
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
