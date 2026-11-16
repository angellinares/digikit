/* The machine of __ID__, declared by builder calls in init: one machine `__NAME__` with one LEVEL parameter. The DSP symbols are _<id>_render,
   _<id>_note_on and _<id>_desc of src/dsp/__NAME__.s. Parameter word 0 is the first word the DSP reads. */
#include <dk/core/module.h>
#include "module_ids.h"
/* Trace marks for C code: see sdk/README.md, "Tracing" (`<dk/core/trace.h>`, `DK_TRACE_LOG`). The DSP side uses `DK_STEP` of dk_trace.inc. */

static int32_t init(dk_module_ctx *ctx)
{
    dk_machine *m =
        dk_module_ctx_add_machine(ctx, DK_MACHINE_ID___UPPER__, "__TITLE__", "__SHORT__");
    dk_page *p = dk_machine_add_page(m, "__TITLE__", "__SHORT__");
    dk_param *level = dk_page_add_param(p, "Level", "LEVEL", 127);
    /* The range of a parameter is 0 to 127 until set: dk_param_range(level, 0, 100); */
    (void)level;
    /* A second page (and its first parameter, the next word for the DSP): dk_page *p2 = dk_machine_add_page(m, "Tone", "TON");
       dk_page_add_param(p2, "Tone", "TONE", 64); each page and parameter adds to the rows in [uses.*] of module.toml. */
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
