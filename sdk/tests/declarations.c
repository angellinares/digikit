/* Compile real module declarations through the public headers, without patchkit,
   firmware, DSP toolchains or a device. */
#include <stdio.h>
#include "../../tests/fixtures/modules/sine/src/module.c"
#include "../../tests/fixtures/modules/dyn-fx-gain-both/src/module.c"

int main(void)
{
    const dk_desc_machine *machine = &dk_desc_machine_sine;
    const dk_desc_param *level = &dk_desc_param_sine_0;
    const dk_desc_fx *fx = &dk_desc_fx_gain;
    const dk_desc_fx_param *gain = &dk_desc_fx_param_gain_0;
    printf("{\"machine\":{\"id\":\"%s\",\"render\":\"%s\",\"note_on\":\"%s\","
           "\"parameter\":{\"owner\":\"%s\",\"word\":%u,\"min\":%d,\"max\":%d,\"default\":%d}},"
           "\"fx\":{\"id\":\"%s\",\"process\":\"%s\",\"mix\":%u,\"route\":%u,\"routes\":%u,"
           "\"parameter\":{\"owner\":\"%s\",\"word\":%u,\"min\":%.2f,\"max\":%.2f,\"default\":%.2f}"
           "}}\n",
           machine->id, machine->render, machine->note_on, level->machine, level->word, level->min,
           level->max, level->def, fx->id, fx->process, fx->mix, fx->route, fx->routes, gain->fx,
           gain->word, gain->min, gain->max, gain->def);
    return 0;
}
