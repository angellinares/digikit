/* <dk/page.h>: what <dk/fx.h> and <dk/machine.h> share. Hand-written.
   Layouts come from the generated <dk/gen/desc.h> (from sdk/sdk.toml [desc]).

   A page of encoders for a machine or an effect:

     DK_PAGE(pluck, "PLUCK", "Pluck", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2), DK_KNOB(3));
     DK_PAGE(fshift, "FSH", "Freq Shifter", DK_KNOB_AT(0, 0));

   The records go to the .dk.desc section. `patchkit` reads them; the device never sees them.
   Text longer than its field is a compile error. */
#ifndef DK_PAGE_H
#define DK_PAGE_H
#include <dk/gen/desc.h>

/* The record section. A host build on macOS (unit tests) needs a Mach-O "segment,section" name. */
#if defined(__APPLE__)
#define DK_DESC __attribute__((section("__DATA,__dk_desc"), used, aligned(4)))
#else
#define DK_DESC __attribute__((section(".dk.desc"), used, aligned(4)))
#endif

#if defined(__clang__)
#define DK_DESC_OVERRIDE_BEGIN \
    _Pragma("clang diagnostic push") _Pragma("clang diagnostic ignored \"-Winitializer-overrides\"")
#define DK_DESC_OVERRIDE_END _Pragma("clang diagnostic pop")
#else
#define DK_DESC_OVERRIDE_BEGIN \
    _Pragma("GCC diagnostic push") _Pragma("GCC diagnostic ignored \"-Woverride-init\"")
#define DK_DESC_OVERRIDE_END _Pragma("GCC diagnostic pop")
#endif

/* Paste two tokens after expanding them (for unique variable names). */
#define DK_CAT_(A, B) A##B
#define DK_CAT(A, B) DK_CAT_(A, B)

/* A page of encoders. OWNER is the machine or effect id. The rest: DK_KNOB / DK_KNOB_AT entries, up to 8. */
#define DK_PAGE(OWNER, NAME, TITLE, ...)                                        \
    static const dk_desc_page DK_CAT(dk_desc_page_##OWNER##_, __LINE__) DK_DESC = { \
        .kind = DK_DESC_PAGE, .size = sizeof(dk_desc_page), .owner = #OWNER,    \
        .name = NAME, .title = TITLE, .knob = {__VA_ARGS__}}

/* The encoder of parameter WORD, at the lowest free position. */
#define DK_KNOB(WORD) {.used = 1, .word = (WORD), .position = DK_POS_AUTO}

/* The encoder of parameter WORD, at position POS (0 .. 7). */
#define DK_KNOB_AT(WORD, POS) {.used = 1, .word = (WORD), .position = (POS)}

/* The glyph of parameter WORD of OWNER (a machine id): the look, value text and step of the knob. GLYPH is one of these; the parameter's range
   must fit it, or patchkit refuses the module:
     DK_GLYPH_ENCODER  0 .. 127
     DK_GLYPH_FADER    0 .. 127
     DK_GLYPH_PAN      0 .. 127 (centre 64, shows L/R)
     DK_GLYPH_ONOFF    0 .. 1
     DK_GLYPH_TUNE     4 .. 124 (centre 64, one unit = one semitone)
   A parameter without a record gets DK_GLYPH_ENCODER. Glyphs on effect parameters are not supported yet. At most one per parameter. */
#define DK_PARAM_GLYPH(OWNER, WORD, GLYPH)                                              \
    static const dk_desc_param_glyph dk_desc_param_glyph_##OWNER##_##WORD DK_DESC = {   \
        .kind = DK_DESC_PARAM_GLYPH, .size = sizeof(dk_desc_param_glyph),               \
        .owner = #OWNER, .word = (WORD), .glyph = (GLYPH)}

#endif
