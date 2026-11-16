/* The machine of test/dyn-dn2-pages, declared by builder calls in init: machine `dyn_pages` with two pages, PGA (three knobs) and PGB (two knobs). The DSP is
   test/sine's (`dsp = "sn"` in module.toml): it reads parameter word 0 only. */
#include <dk/core/module.h>
#include "module_ids.h"

static int32_t init(dk_module_ctx *ctx)
{
    dk_machine *m = dk_module_ctx_add_machine(ctx, DK_MACHINE_ID_DYN_PAGES, "DynPages", "DPG");
    dk_page *a = dk_machine_add_page(m, "Pages A", "PGA");
    dk_page_add_param(a, "Pg Alpha", "ALPH", 32);
    dk_param_range(dk_page_add_param(a, "Pg Beta", "BETA", 16), 0, 63);
    dk_param_range(dk_page_add_param(a, "Pg Gamma", "GAMA", 112), 1, 127);
    dk_page *b = dk_machine_add_page(m, "Pages B", "PGB");
    dk_page_add_param(b, "Pg Epsilon", "EPSL", 8);
    dk_page_add_param(b, "Pg Zeta", "ZETA", 127);
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
