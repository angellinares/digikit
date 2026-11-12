/* <dk/fx/fx.h>: the effect contract for content modules.
   Hand-written. Layouts come from the generated <dk/gen/fx.h> and <dk/gen/desc.h> (from sdk/sdk.toml).

   Declare each effect in a C file of the module:

     #include <dk/fx/fx.h>
     DK_FX(fshift, .process = "_fs_process", .desc = "_fs_desc", .mix = 127,
           .route = DK_ROUTE_SEND, .routes = DK_ROUTE_SEND | DK_ROUTE_MASTER);
     DK_FX_PARAM(fshift, 0, "Freq Shift", "SHIFT", DK_BIPOLAR, -64, 63, 0);
     DK_PAGE(fshift, "FSH", "Freq Shifter", DK_KNOB_AT(0, 0));

   The records go to the .dk.desc section. `patchkit` reads them; the device never sees them.
   MIX defaults to 127. Text longer than its field is a compile error. */
#ifndef DK_FX_H
#define DK_FX_H
#include <dk/gen/fx.h>
#include <dk/page/page.h>

/* An effect: ID is a C identifier and becomes the effect id. The rest are dk_desc_fx field initializers. */
#define DK_FX(ID, ...)                                                                             \
    DK_DESC_OVERRIDE_BEGIN                                                                         \
    static const dk_desc_fx dk_desc_fx_##ID DK_DESC = {                                            \
        .kind = DK_DESC_FX, .size = sizeof(dk_desc_fx), .id = #ID, .mix = 127, __VA_ARGS__};       \
    DK_DESC_OVERRIDE_END

/* Parameter word WORD of effect FX. The effect reads a float from MIN to MAX. DISPLAY is DK_PERCENT or DK_BIPOLAR. */
#define DK_FX_PARAM(FX, WORD, NAME, SHORT, DISPLAY, MIN, MAX, DEF)                                 \
    static const dk_desc_fx_param dk_desc_fx_param_##FX##_##WORD DK_DESC = {                       \
        .kind = DK_DESC_FX_PARAM,                                                                  \
        .size = sizeof(dk_desc_fx_param),                                                          \
        .fx = #FX,                                                                                 \
        .word = (WORD),                                                                            \
        .display = (DISPLAY),                                                                      \
        .min = (MIN),                                                                              \
        .max = (MAX),                                                                              \
        .def = (DEF),                                                                              \
        .name = NAME,                                                                              \
        .short_name = SHORT}

#endif
