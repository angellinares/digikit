/* <dk/page/page.h>: what <dk/fx/fx.h> and <dk/machine/machine.h> share. Hand-written.
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

/* Native callback page v1 (machine contract 1.4). The normal DK_PAGE stays page zero.
   PARAM bindings edit existing words through the firmware; LOCAL bindings never edit a stored word.
   Canvas coordinates are local to its clipped content rectangle. Functions are valid only during draw.
   Callbacks run in the UI task. Do not retain the frame/event or call canvas functions outside draw. */
typedef struct dk_page_canvas_v1 {
    uint32_t width, height;
    void (*text)(int32_t x, int32_t y, const char *text);
    void (*fill_rect)(int32_t x0, int32_t y0, int32_t x1, int32_t y1, int32_t color);
} dk_page_canvas_v1;

typedef struct dk_page_frame_v1 {
    uint32_t version, size, track;
    int32_t params[8]; /* current native PARAM values, indexed by encoder position */
    dk_page_canvas_v1 canvas;
    /* Snapshot of this machine's declared parameters, in declaration order.
       word_count is 0..8; registration rejects a larger snapshot. Independent
       of encoder bindings; read-only and valid for this draw only. */
    uint32_t word_count;
    int32_t word_values[8];
    int32_t level; /* read-only native track level; independent of A-H bindings */
} dk_page_frame_v1;

typedef struct dk_page_event_v1 {
    uint32_t version, size, track, control;
    int32_t delta; /* signed detents, independent of PARAM coarse/p-lock editing */
    uint32_t coarse;
} dk_page_event_v1;

#define DK_NATIVE_PAGE_V1(OWNER, TITLE, DRAW, EVENT, ...)                         \
    void DRAW(const dk_page_frame_v1 *frame);                                    \
    void EVENT(const dk_page_event_v1 *event);                                   \
    static const dk_desc_native_page DK_CAT(dk_desc_native_##OWNER##_, __LINE__) DK_DESC = { \
        .kind = DK_DESC_NATIVE_PAGE, .size = sizeof(dk_desc_native_page),         \
        .version = 1, .owner = #OWNER, .title = TITLE, .draw = #DRAW, .event = #EVENT, \
        .binding = {__VA_ARGS__}}
/* Opt-in replacement pair (machine contract 1.5): declare exactly page 0 and
   page 1 for OWNER. The ordinary DK_PAGE remains the parameter/storage schema;
   both visible pages use these callbacks. Never mix with DK_NATIVE_PAGE_V1. */
#define DK_NATIVE_PAGE_V2(OWNER, PAGE, TITLE, DRAW, EVENT, ...) \
    DK_NATIVE_PAGE_V2_FLAGS(OWNER, PAGE, 0, TITLE, DRAW, EVENT, __VA_ARGS__)

/* Page-zero stock overlay: native widgets draw first, then the callback. The
   frame ABI is unchanged. Canvas stays clipped to 99x53 local pixels. Only
   rectangles explicitly filled by the callback are composited; clear each
   intended overlay area before drawing text/glyphs there. Other stock pixels,
   including LEV and page dots outside the canvas, remain intact. Renderer
   internals are adapter-owned and may change without changing this contract. */
#define DK_PAGE_STOCK_OVERLAY 1u
/* Replacement page one may expand left to 107x53 pixels (global x17..123).
   Mutually exclusive with STOCK_OVERLAY; ordinary pages remain 99x53. */
#define DK_PAGE_WIDE_CANVAS 2u
#define DK_NATIVE_PAGE_V2_FLAGS(OWNER, PAGE, FLAGS, TITLE, DRAW, EVENT, ...)     \
    void DRAW(const dk_page_frame_v1 *frame);                                  \
    void EVENT(const dk_page_event_v1 *event);                                 \
    static const dk_desc_native_page_v2 DK_CAT(dk_desc_native_v2_##OWNER##_, __LINE__) DK_DESC = { \
        .kind = DK_DESC_NATIVE_PAGE_V2, .size = sizeof(dk_desc_native_page_v2),  \
        .version = 2, .page = (PAGE), .flags = (FLAGS), .owner = #OWNER, .title = TITLE,           \
        .draw = #DRAW, .event = #EVENT, .binding = {__VA_ARGS__}}
#define DK_PAGE_PARAM(WORD, POS) {.kind = 1, .value = (WORD), .position = (POS)}
#define DK_PAGE_LOCAL(ID, POS) {.kind = 2, .value = (ID), .position = (POS)}

/* The glyph of parameter WORD of OWNER (a machine id): the look, value text and step of the knob. GLYPH is one of these; the parameter's range
   must fit it, or patchkit refuses the module:
     DK_GLYPH_ENCODER  0 .. 127
     DK_GLYPH_FADER    0 .. 127
     DK_GLYPH_PAN      0 .. 127 (centre 64, shows L/R)
     DK_GLYPH_ONOFF    0 .. 1
     DK_GLYPH_TUNE     4 .. 124 (centre 64, one unit = one semitone)
     DK_GLYPH_PLAY     0 .. 3 (REV, REV.L, FWD.L, FWD; the Digitone II shows 0 .. 3)
   A parameter without a record gets DK_GLYPH_ENCODER. Glyphs on effect parameters are not supported yet. At most one per parameter. */
#define DK_PARAM_GLYPH(OWNER, WORD, GLYPH)                                              \
    static const dk_desc_param_glyph dk_desc_param_glyph_##OWNER##_##WORD DK_DESC = {   \
        .kind = DK_DESC_PARAM_GLYPH, .size = sizeof(dk_desc_param_glyph),               \
        .owner = #OWNER, .word = (WORD), .glyph = (GLYPH)}

#endif
