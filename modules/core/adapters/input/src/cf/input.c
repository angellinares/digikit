/* core/input: press keys and turn encoders from software, through the firmware's own panel decoders (docs/plans/remote-control.md).
 *
 * The panel MCU sends 2-byte packets on a UART (key: 0x20 | channel, then the 8-bit mask of that channel; encoder: 0x30 | (encoder - 1), then a signed
 * delta). The firmware turns a packet into events with two plain C functions, `f(channel, new mask)` and `f(encoder - 1, delta)`; the first keeps the
 * previous mask of every channel in firmware RAM, which chords, repeat and the key-down query read. This module calls those two decoders, so a key
 * pressed here is exactly a key the panel reported, and nothing in the firmware's event path is bypassed.
 *
 * Interface for modules (C ABI, named imports of the runtime's table; 0 is ok, negative an error):
 *   int input_key(u32 code, u32 down);     a key code (the device file, [panel]) pressed or released
 *   int input_turn(u32 encoder, int delta); encoder 1..9, delta in detents (split into parts of at most 127)
 *
 * Device facts (the code layout, the backend) come from the device file through `input_cfg` (device.s). A backend is one set of the two functions; the
 * profile names it ([input] backend), and a device with another path to its events adds a backend here.
 *
 * The decoders run with the interrupts masked (SR 0x2700), the discipline of the runtime's other calls into the firmware's queue code, and are called
 * from the MIDI task, never from the UART byte callback (it shares header state with the interrupt).
 *
 * A key is passed to the decoder only when it changes the channel's mask. The panel's next real packet for that channel carries the whole mask and
 * overrides what this module set; on the emulator no panel sends any. A turn is accumulated by the firmware and flushed by its periodic tick, which
 * clamps a flush to +-30: the caller paces large turns. */

#include <stdint.h>

/* The opaque handle types of the service imports (sdk/sdk.toml [handle]); <dk/gen/imports.h> is not included: it also declares the old import names with the SDK's integer types. */
typedef struct dk_ctx dk_ctx;
typedef struct dk_input dk_input;

typedef unsigned int u32;
typedef unsigned char u8;

#define BACKEND_PANEL_UART 1u

#define IN_OK 0
#define IN_BAD_CODE (-1)
#define IN_QUEUE_FULL (-2)
#define IN_NO_BACKEND (-3)
#define IN_BAD_ARG (-4)

/* the device facts (device.s) */
extern const u32 input_cfg[];
#define CFG_BACKEND 0
#define CFG_LINEAR 1
#define CFG_ENCODERS 2
#define CFG_EXC_N 3
#define CFG_EXC 4        /* 8 x (code, channel, bit) */
#define CFG_BUTTON_CH 28 /* the channels the button decoder has tables for */

/* firmware (anchors by hash in the port files): the decoders, and the literals that locate their state */
extern void input_fw_key(u32 channel, u32 mask);
extern void input_fw_turn(u32 encoder0, int delta);
extern volatile u32
    input_mask_lit; /* the literal: address of the previous mask of channel 0 (one byte per channel) */
extern volatile u32 input_queue_lit; /* the literal: address of the UI queue object's pointer */

/* events one decoder call may add at most (a key: one per changed bit, here one bit; a turn: one) plus a margin */
#define QUEUE_MARGIN 4u

static inline u32 sr_mask(void)
{
    u32 old;
    __asm__ volatile("move.w %%sr,%0\n\tmove.w #0x2700,%%sr" : "=d"(old) : : "memory", "cc");
    return old;
}

static inline void sr_restore(u32 old)
{
    __asm__ volatile("move.w %0,%%sr" : : "d"(old) : "memory", "cc");
}

/* free places in the UI queue, or 1000 when the queue cannot be read (then the decoder's own check stands). Field offsets as the runtime's boot hook reads them. */
static u32 queue_room(void)
{
    u32 q = input_queue_lit;
    const volatile u32 *w;
    u32 count, mask;
    if (q < 0x40000000u || (q & 3u) != 0u)
        return 1000u;
    w = (const volatile u32 *)q;
    count = w[1];
    mask = w[4];
    if (mask > 0xffffu || count > mask + 1u)
        return 1000u;
    return mask + 1u - count;
}

/* ---- backend panel-uart ---- */

/* code -> (channel, bit): 1 .. 8 * linear is linear, the others come from the exceptions; 0 when the code is not a key */
static int panel_code(u32 code, u32 *ch, u32 *bit)
{
    u32 linear = input_cfg[CFG_LINEAR];
    u32 n = input_cfg[CFG_EXC_N];
    u32 i;
    if (code >= 1u && code <= linear * 8u) {
        *ch = (code - 1u) >> 3;
        *bit = (code - 1u) & 7u;
        return *ch < input_cfg[CFG_BUTTON_CH];
    }
    for (i = 0; i < n && i < 8u; i++) {
        const u32 *e = &input_cfg[CFG_EXC + 3u * i];
        if (e[0] == code) {
            *ch = e[1];
            *bit = e[2];
            /* the decoder has no bounds check on the channel: never pass one outside its tables */
            return *ch < input_cfg[CFG_BUTTON_CH];
        }
    }
    return 0;
}

static int panel_key(u32 code, u32 down)
{
    u32 ch, bit, old, now, sr;
    volatile u8 *mask;
    if (!panel_code(code, &ch, &bit))
        return IN_BAD_CODE;
    mask = (volatile u8 *)input_mask_lit;
    old = mask[ch];
    now = down ? (old | (1u << bit)) : (old & ~(1u << bit));
    if (now == old)
        return IN_OK;
    if (queue_room() < QUEUE_MARGIN)
        return IN_QUEUE_FULL;
    sr = sr_mask();
    input_fw_key(ch, now);
    sr_restore(sr);
    return IN_OK;
}

static int panel_turn(u32 encoder, int delta)
{
    u32 sr;
    if (encoder < 1u || encoder > input_cfg[CFG_ENCODERS])
        return IN_BAD_CODE;
    while (delta != 0) {
        int part = delta > 127 ? 127 : (delta < -127 ? -127 : delta);
        if (queue_room() < QUEUE_MARGIN)
            return IN_QUEUE_FULL;
        sr = sr_mask();
        input_fw_turn(encoder - 1u, part);
        sr_restore(sr);
        delta -= part;
    }
    return IN_OK;
}

/* ---- the interface ---- */

int input_key(u32 code, u32 down)
{
    if (input_cfg[CFG_BACKEND] == BACKEND_PANEL_UART)
        return panel_key(code, down);
    return IN_NO_BACKEND;
}

int input_turn(u32 encoder, int delta)
{
    if (input_cfg[CFG_BACKEND] == BACKEND_PANEL_UART)
        return panel_turn(encoder, delta);
    return IN_NO_BACKEND;
}

/* ---- the service imports of <dk/gen/imports.h> (docs/plans/sdk.md section 5.1) ---- */

static int input_token; /* the dk_input handle: only a token */

int32_t dk_input_open(dk_ctx *ctx, dk_input **out)
{
    if (!ctx || !out)
        return -48; /* DK_E_CONTRACT */
    *out = (dk_input *)&input_token;
    return 0;
}

int32_t dk_input_key(dk_input *inp, uint32_t code, uint32_t down)
{
    (void)inp;
    return input_key(code, down);
}

int32_t dk_input_turn(dk_input *inp, uint32_t encoder, int32_t delta)
{
    (void)inp;
    return input_turn(encoder, delta);
}
