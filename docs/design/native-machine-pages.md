# Native machine callback pages

This opt-in API has two modes sharing the bounded frame/event v1 callback ABI.
Legacy `DK_NATIVE_PAGE_V1` adds one page after the ordinary `DK_PAGE` (machine
contract 1.4). `DK_NATIVE_PAGE_V2` replaces the visible pages with exactly two
custom pages, indexes 0 and 1 (machine contract 1.5). Existing machines keep their
page count, descriptors and parameter slots unless they explicitly opt in.

`DK_NATIVE_PAGE_V1(owner, title, draw, event, ...)` declares callbacks and up to eight
bindings. `DK_PAGE_PARAM(word, position)` reuses a declared word of that owner;
`DK_PAGE_LOCAL(id, position)` declares UI-only state. Positions are encoders A–H,
numbered 0–7. The neutral `test/native-page` fixture binds START at A, ZOOM at C,
and END at H. Its ordinary declarative page is retained. The separate
`test/native-pages` fixture proves replacement pages and action routing.

The runtime validates the version, owner, binding positions, existing PARAM
references and callback offsets before publishing the page. It resolves PARAM
references to the same native parameter codes used by the ordinary page. The
adapter copies titles and bindings into its private registry, and removal clears
callbacks before the runtime releases module code. No stock object grows.

The draw callback receives `const dk_page_frame_v1 *`: a track, current PARAM
values indexed by encoder position, and a bounded canvas. Its appended
`word_count`/`word_values[8]` snapshot reads this machine's existing parameters in
declaration order independently of encoder bindings. The runtime copies only
codes owned by the machine (at most eight), and the adapter reads them through
the native getter on the current track for each draw. Shot reads SMP at word 0.
The appended read-only `level` is the native raw track level (unit * 256), read
through the existing LEVEL getter independently of all encoder bindings. Neither
snapshot reserves a knob. Version and size let
content check the context. Canvas coordinates are local to its reported width
and height. Text and rectangles clip to that canvas; content must not retain the
context or its function pointers outside the draw callback. Firmware Bitmap/font
representation and screen origin remain private to the adapter.

The event callback receives `const dk_page_event_v1 *` for LOCAL turns only,
including track, local control ID, signed detents and the native coarse flag.
Content owns local state and its per-track policy. The adapter invalidates the
view after a LOCAL event. LOCAL and unbound turns return consumed; PARAM turns
continue through the existing native handler exactly once, preserving native
acceleration, coarse edits and p-lock handling. This API does not call the
click-scoped `param_set` API for turns.

The descriptor is SDK record kind 11, version 1. The packed register section uses
kind 6, length 0x90: version, owner key, draw/event image offsets, title[24], then
eight (kind, value, encoder) u32 triples. Kind 1 values are file PARAM indexes
until runtime allocation resolves them to native codes; kind 2 values are local
IDs. Unused triples must be zero and follow all used entries. No extra DSP word,
ordinary PAGE row or parameter row is allocated for the callback page.

Replacement v2 uses SDK descriptor kind 12 and packed register kind 7, length
0x98. Its common header/title/callback fields match v1; page-index and flags words follow
the title before the bindings. Exactly one record for page 0 and one for page 1
are required per owner; missing/duplicate indexes and mixed v1/v2 records are
rejected by both packer and runtime. The ordinary `DK_PAGE` remains the validated
parameter/storage schema. The adapter retains at most two callbacks/descriptors
per machine type; it reports two visible pages for v2 and keeps v1's append-only
count. PARAM click actions follow the custom binding through the same mapper.
The physical LEVEL encoder retains its native path.

Current ports are DT2 1.17 and DN2 1.12. [D] The source wrapper/dispatcher return
seams allow a nonzero D0b result to consume a turn, and the native code mapper
routes declarative PARAM bindings through existing code-to-storage resolution.
The font is a live RAM object; its address is read from a hashed immediate
operand in the native parameter-page draw code. The callback canvas uses private
scratch planes and a copied Bitmap header, then blits only its bounded area.
Device acceptance remains the proof for navigation, drawing and input routing;
static checks alone do not establish those behaviors.

Shot uses replacement v2 on both devices. Its control page binds TUNE A, PLAY B, SMP D, START E and END H, preserving
the stock START/END widgets and left LEV fader. The overlay supplies code-native
tune/sample boxes and portable PLAY. Empty selected sample slots are dotted;
there is no center status text. TUNE keeps Shot's 3/8-semitone scale. SMP's click
opens the picker from D; the ready sample box shows its local slot index. PLAY's
portable direction/loop mark is drawn with our own canvas primitives.

The waveform page binds START E, local ZOOM G and END H, with labels and values
along the bottom and no filename footer; START/END numbers are centered under
E/H, with ZOOM centered near G. `<dk/sample_region.h>` renders at most
512 zero-inclusive stereo min/max bins, a permanent center line and contrasting
opposite trim flags. Zoom is a Q8 ratio 256..1024 (1x..4x), 1/64x per detent,
following START; off-viewport handles are pinned to its edge. The original
three-step renderer helper remains available for existing callers.

Peak data is built on the loader worker: DN2 accumulates its existing stream;
DT2 reads source PCM in 4 KiB chunks after the stock load. No draw callback scans
PCM. An overview failure keeps a playable DT2 sample READY; the graph still has
its center line and handles. Failed tokens are remembered, with retry only for
a new slot generation. Backing storage remains 128 KiB on DN2 (64 blocks),
32 KiB on DT2 (16 slots), plus DT2's 4 KiB read buffer. No additional peak memory
is needed for fractional zoom.

API 1.7 `sample_pool_peaks` validates the READY token, then copies at most 512
native-endian int16 min/max pairs. Stale tokens and non-READY entries cannot
expose earlier peaks. The optional backing `dkpool_peaks` also validates its
private handle token. Shot caches by slot and public token and requests peak
copies only on the waveform page; the control page can fetch its basename alone.


V2 page zero may opt into `DK_PAGE_STOCK_OVERLAY`. The hook lets stock drawing
finish before calling the bounded overlay renderer; only callback fill rectangles
are composited. Default V2 and V1 behavior is unchanged. Unknown flags and
page-one overlays are rejected. The frame ABI stays unchanged. The separate
`<dk/widgets.h>` helpers remain available for modules drawing their own pages;
Shot uses stock widgets on page zero and its custom waveform on page one.

Replacement page one may request `DK_PAGE_WIDE_CANVAS` (flag 2). The adapter
provides 107×53 pixels at global x17..123; default and stock-overlay pages
remain 99×53 at x25..123. This reuses the flags field and frame canvas dimensions,
without changing either ABI layout. The packer, runtime and adapter reject
unknown flags, a wide page zero, an overlay page one, and their combination.
Shot uses this for a global x17..111/y6..36 waveform centered at x64. Its E/G/H
labels and values move eight local pixels right to preserve their global
positions. The SDK region rectangle clips every waveform and marker write.
