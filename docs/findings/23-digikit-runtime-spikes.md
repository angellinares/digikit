# 23. digikit runtime: Phase 0 spikes (S1 to S4)

Plan: `docs/plans/digikit-runtime.md`. One agent per spike, 2026-10-16; static except S1b (a toolchain build) and
S5 (emulator instruments). A second agent checked S2 and S3 against the DT2 1.16 and DN2 1.11 images: bullets
marked **[V]** are confirmed, **[C]** corrects the first reading; everything else is **[D]** at most. Images:
DT2 1.16 (`dt2-1.16-emac`), DN2 1.11 where stated. Nothing was flashed.

## S1: Rust on the ColdFire **[D]**

- rustc 1.98.1 (LLVM 22.1.8) has the tier-3 target `m68k-unknown-none-elf` and builds `core` with
  `-Zbuild-std=core`. LLVM CPUs: M68000 to M68060 and generic. No ColdFire CPU and no feature that removes the
  68k-only forms.
- A test crate (byte and halfword copies, u16 struct walk, a relocation applier, a function-pointer table, a u8
  match, 64-bit multiply, an extern "C" entry) was built for M68000 and M68020 at opt-level s and 2, and the
  emitted assembly was assembled with `m68k-elf-as -mcpu=54418`. Rejected lines: 11 to 12 of about 200
  instructions per build, 27 of 343 with `core::fmt`.
- Rejected classes: byte and word ALU forms (`sub.b #7,%d2`, `eori.w`, `cmpi.b #0,(1,%a0)`), rotates and word
  shifts (`rol.w #8`, `rol.l #5`, `lsr.w #3`), immediate to memory with displacement (`move.l #7,(4,%sp)`),
  memory to indexed memory (`move.b (16,%sp),(0,%a0,%d1)`).
- Calling convention matches gcc for integer arguments and results (stack arguments, d0 result, d2-d7/a2-a6
  saved). Pointer results (LLVM: a0), stack alignment and the libcall set (`__muldi3`, `__umodsi3`) are **[O]**.
- `rustc_codegen_gcc` with a cross libgccjit for m68k-elf was not tried. Estimate: 1 to 3 agent days to a first
  build, medium chance of a stable result **[O]**.
- Result: stock rustc/LLVM cannot target the ColdFire. S1b below does it through GCC.

## S2: the SETTINGS menu **[V]** unless marked

DT2 1.16:

- The view is `DigisharcSettingsMenu` (a `VerticalMenuView` and an Observer), constructor `0x400a5d72`, created
  once with `operator new(0x230)` (make_shared at `0x401af8c0`) inside the main screen view.
- The item list is built at run time by `FUN_400a59a0`: clear (vtable slot +0x14), add each item (slot +8,
  `Menu::addItem` `0x40114f9a`), restore the selection (slot +0x1c). It runs from the constructor (`0x400a616e`)
  and from one Project observer callback (the thunk `0x400a5d64`, `bra.w` at `0x400a5d6e`). **[C]** The
  constructor registers two callbacks, but the first (invoker `0x400a5262`) does not rebuild. Nothing else calls
  or jumps into the rebuild.
- Items in order: PROJECT, SONG, PATTERN, MIDI CONFIG, SYSEX DUMP, AUDIO ROUTING, OUTBOX 8 (conditional; **[C]**
  not "BY OUTBOX 8"),
  PERSONALIZE, SYSTEM. Submenus are built in the constructor and held as `shared_ptr<View>` members at
  `this+0x1c8..0x220`. **[C]** Two of the eleven slots, `this+0x1f8` and `this+0x208`, are zeroed and never used,
  so one can hold our view.
- `MenuItem` (`0x54` bytes, constructor `0x40115764`): vtable `0x4021ccdc`, label `std::function` at +0x0c,
  action `std::function` at +0x1c, the icon (a `Bitmap` copied by `FUN_401156dc`) at +0x2c **[C]**, a fourth
  `std::function` at +0x3c. Builder
  `FUN_401158b0(item, const char *label, &action, const Bitmap *icon, &fn4)`.
- `std::function` is 16 bytes: data at +0, manager at +8 (null = empty), invoker at +0xc. `FUN_400a537c` is a
  manager that can be copied.
- Opening a submenu: the action copies the member `shared_ptr` (`FUN_401af3b0`) and calls `FUN_4011a2b2(this,
  &sp)`, which pushes the view (`FUN_4011bdaa`).
- `VerticalMenuView` constructor `0x40119c8c` `(this, const char *name, int reserve, const std::string *title,
  const Bitmap *icon)` (callers pass a sixth argument that it ignores; **[C]**), vtable `0x4021d83c`, object
  `0x1ac` bytes (from the derived classes). Items are a
  `vector<shared_ptr<MenuItem>>` at +4/+8/+0xc. Text is drawn by `FUN_40113a80(bitmap, font, x, y, w, cstr)` (46
  call sites).
- No checkbox class. Toggle rows (for example `KitVoiceMuteConfigMenuView` `0x4007cc34`) are plain `MenuItem`s in
  a `VerticalSettingsMenuView` (`0x4011a9a8`) with their own draw callbacks.
- ABI: gcc m68k cdecl, `this` is the first stack argument; struct results through a hidden pointer in a0.
  `std::string` is the pre-C++11 copy-on-write layout (one pointer, header at -12); construct with
  `FUN_401e7a64`, destroy with `FUN_401e5fb4`. `operator new` is `FUN_4011e674`.
- Smallest patch: one hook at the tail of `FUN_400a59a0` (the 12 bytes at `0x400a5d58`: `movem.l`, `lea`, `rts`)
  that calls our function with `this` (live in a2). It adds a DIGIKIT `MenuItem` after SYSTEM on every rebuild.
  Our action builds a stock `VerticalMenuView` on first use, keeps it in the spare slot `this+0x1f8` and pushes
  it with `FUN_4011a2b2`. No new vtable. Confidence that this works on the first emulator run: about 70 %
  **[D]**.
- Open **[O]**: the `shared_ptr` helper at `0x40119dda` (name and signature), destructor paths of our
  `MenuItem` and view objects, `Bitmap` lifetime.
- DN2 1.11 has the same structures: constructor `0x4009fe4e`, rebuild `0x4009fa7c`, the same 12-byte tail at
  `0x4009fe34`, `VerticalMenuView` `0x4011b600`, `MenuItem` `0x401170d8`, `operator new` `FUN_4011ffe8`. The
  other helpers were not compared one by one.

## S3: reading files from our code **[V]** unless marked

DT2 1.16. A C-level API exists below the C++ classes. Each entry point takes the filesystem mutex
(`0x46f4deb0`, `FUN_400015a0` lock, `FUN_400016d2` unlock) and returns -1 when the filesystem is not mounted
(`_DAT_44f2bd68`). There is no filesystem task or message queue (**[D]**, an absence); the UI task already calls this layer (the sample
picker reads directories), so it can be called from the UI task. Not from an interrupt. A read blocks the caller
while the card is read (time per 32 KiB page **[O]**).

| Function | Address | Notes |
| --- | --- | --- |
| open | `FUN_40158626(path, mode, h)` | mode "r" or "w"; 0 = ok; handle `{id, pos, wr, open}` (16 bytes) |
| read | `FUN_4015832e(buf, len, h)` | bytes read, 0 at end; -1 only when unmounted or for a bad argument or handle; no length cap |
| size | `FUN_4015ba22(id)` | 0xFFFFFFFF on error |
| close | `FUN_401585e4(h)` | 0 = ok |
| open directory | `FUN_40156fc4(path, it)` | iterator `{dir, page, off, idx}` (16 bytes) |
| next entry | `FUN_40157072(it, ent)` | 1 = entry, 0 = end, -1 unmounted; entry 0x114 bytes: id +0, type +4 (1 = directory), name +0x14 |

- The sample loader `FUN_40154540` uses the same open/size/read loop (0x8000-byte reads), opening by a 16-byte
  reference instead of a path **[D]**.
- No extension or format check in open or read. The firmware owns only "/factory" (read-only, RAM-resident id
  0x1000000); nothing rejects other top-level names **[D]**. A `/digikit` folder will also show in the sample picker.
- The builder (packages/emulator/plusdrive) writes only a flat root of native samples (`validate_native` in
  `image.rs` rejects other files). The runtime needs nested directories and plain files. The name hash page
  (0x10000) must be correct because path lookup `FUN_401572e0` binary-searches it; readdir also needs the listing
  page 0x10001.
- DN2 1.11: no path filesystem was found. The superblock magic 0x656b4653 and its sector constants are not in the
  MAIN OS, and there are no `File`, `Directory` or `FileSystemDirectory` classes and no "/factory". The
  `MidiRpcFsRaw*` classes exist. DN2 stores projects, sounds and kits in the MmcFs slot pools like DT2; what it
  lacks is the folder filesystem that DT2 uses for samples. Where DN2 could store module files is **[O]** (plan
  spike S6).

## S4: ColdFire to SHARC after boot **[D]**

- DT2: the sample upload path (finding 15, finding 04) can write arbitrary bytes into DSP DDR. `FUN_400cd638`
  sends tagged transfers over link port 0; on the DSP, `FUN_1c7e1f` dispatches by tag, and a 4 KiB page tag
  (`< 0x19200`) is copied by `FUN_1c403c` to `0x8422B9C8 + (page << 12)`. The ColdFire chooses the page; the DSP
  does not check the content.
- Execution from DDR (L3) on this part is not confirmed **[O]**. Plan: a static DSP stub in L2, installed at patch
  time, copies the bytes from DDR into free L2, invalidates the instruction cache for the range and calls the
  code. The SHARC+ manual requires the invalidate: range invalidation with `CAINV_START0`/`CAINV_END0` (64-byte
  aligned) and `FLUSH`, or a full cache flush (sc58x-2158x-prm, printed pages 8-1 and 8-13, and the overlay note on
  pp. 154-155).
- Where the stub polls is **[O]**: the tail of the per-block dispatch is the candidate. Measured idle share per
  block is 31 to 44 % on DT2 and 50 to 67 % on DN2 (finding 06).
- DN2: no sample upload path exists. The only run-time transport is the per-block TX frame (2748 bytes, 1500 per
  second), and its fields are live parameters. The one field measured to have no effect on DN2 output (the
  per-track delay send) would give at most about 48 KB/s and gives up a parameter. No existing channel can be
  reused; S4b below finds one of our own.
- No existing command writes DSP memory at a given address other than the DT2 sample page path. The DDR command
  block at `0x82a00000` (docs/sharc/structure-1.16.md) is a hypothesis.

## S1b: Rust through GCC's back end **[D]**

- `rustc_codegen_gcc` (pinned nightly-2026-10-11, rust-lang/gcc commit 37ea7cd8, a GCC 17 fork) with libgccjit
  built on macOS arm64 for `--target=m68k-elf --enable-host-shared --enable-languages=jit,c`, using the repo's
  m68k-elf `as` and `ld`. Setup took about 75 minutes. Reproduction: `out/tools/codegen-gcc/README-local.txt`
  (local, not committed).
- Local patches: four for libgccjit on macOS (libc++ header clash, `std::_Hash_bytes`, a static initializer that
  read `targetm` hooks before they were set, a stale export list) and three lines in the backend so that
  `-mcpu=54418` also reaches the GCC driver (otherwise only `cc1` saw it and the assembler rejected the ColdFire
  forms).
- The S1 test crate plus ABI probes (about 630 lines of assembly) re-assemble with `m68k-elf-as -mcpu=54418`
  with zero rejected lines (LLVM: 11 to 27). All 25 objects of the static library, `core` included, carry the
  ELF flags `cf, isa C, emac`. Relocations are only `R_68K_32`, which patchkit's linker takes.
- Calling convention against `gcc -mcpu=54418` on equivalent C: the same for integer and pointer arguments and
  results, `u64` in d0:d1, and structs passed by value or returned through a0. Different: an 8-byte struct
  result (C: d0:d1; Rust: through a0). Module interfaces avoid small-struct results.
- Layout: the stock m68k data layout aligns `u64` to 4; GCC C aligns it to 2. The target's data layout needs
  `i64:16:64` to match C.
- Helpers: core references `__muldi3`, `__udivdi3`, `__umoddi3`, soft-float helpers, `memcpy`, `memset`, `memcmp`;
  128-bit helpers come from `compiler_builtins`. The memory functions must come from the runtime.
- The repo's m68k-elf toolchain has no ColdFire multilib: its libgcc `__muldi3` has ELF flags 0 and uses the
  68020 64-bit `mulu.l`, which the ColdFire lacks. This affects C code too; no committed module object uses these
  helpers today.
- A clean release build of `core`, `compiler_builtins` and the crate takes 13 s.
- Open **[O]**: a full link, running the code in the emulator, `alloc`, code quality against C, the backend's
  stability on larger crates.

## S4b: a DSP channel of our own **[D]**

- The DSP never reads the end of the per-block TX frame. DN2 1.11 sends 1344 data words of a 1374-word frame:
  `FUN_400cf7be` writes only the command half of the last 30 PUSHR entries, so their data halves
  (`0x80002510 + i*4 + 2`, i = 1344..1373) carry whatever was stored there. DT2 1.16 sends 0x802 of 0xabc bytes
  (`FUN_400cd2bc(0x802, 0x80005348, 0xabc, 0x8000488c)`, called at `0x4002dd74`), which leaves 698 bytes.
- Tail channel: retarget the `jsr` to the frame builder (DN2 `0x40025e9e`, DT2 `0x4002dd74`; 6 bytes) to a
  wrapper that fills the tail, then calls the stock function. On the DSP, a hook at the block handler (DN2
  `0x1c9d9f`, DT2 `0x1c7501`; the frame pointer is in I3) copies the tail before the stock code runs. About
  84 KB/s on DN2 and 1 MB/s on DT2, no mute, no change to parameter handling. Acknowledge through the DSP record
  that the ColdFire reads back (the ColdFire reads only bytes 0x00-0x38 and 0xa9c-0xabc of it).
- Faster load (optional): replace the whole frame with payload for a few blocks. A first word of 4 or more makes
  the stock block handler take its clear path (`0x1c9dc2`): output is silent, the engine and the parameter copy
  do not run, and the next normal frame resumes at once. About 4 MB/s. Load with the sequencer stopped.
- The DN2 receive ring is at `0x2c29d0`/`0x2c39d0`; the parameter copy (`FUN_1c2712`) copies 672 words to
  `0x25c48c`.
- The DSP blobs of DN2 1.11 and 1.12 are identical, and so are those of DT2 1.16 and 1.17; the ColdFire call
  sites port to the later images at the same addresses.
- Lengthening the frame (6+ constants on both CPUs and the SRAM layout) and restarting only the DSP are not
  worth it.
- Open **[O]**: how the DSP's 32-bit read combines the first two wire units (choose a magic that cannot occur in
  a stock frame and confirm with a captured frame); that no engine write lands in the record bytes used for the
  acknowledgement; the instruction cache invalidate after the copy.
  Built and settled in finding 31 (`core/dsp-link`): the byte order is confirmed on captured frames, the
  acknowledgement cannot go where this section proposed (the DSP rewrites the whole record every block, so the stub
  runs at the end of the handler instead) **[C]**, and the cache sequence is written down but not exercised by the emulator.

## S5: emulator write-watch and flush check **[D]**

- `DIGI_WRITE_WATCH=LO-HI[,..]` reports every write into the ranges from CPU stores and from DMA (eSDHC, DSPI),
  with the instruction count, PC or DMA source, address, size and value; `DIGI_WRITE_WATCH_ALLOW_PC`,
  `DIGI_WRITE_WATCH_AFTER=cacr|ICOUNT`, `DIGI_WRITE_WATCH_OUT`, `DIGI_WRITE_WATCH_LIMIT`. `DIGI_FLUSH_CHECK=1`
  reports a fetch of bytes written after the caches were enabled and not pushed and invalidated since (dirty
  bytes are tracked per 16-byte line).
  Details: docs/INSTRUMENTATION.md. Code: packages/emulator/boot/src/watch.rs (on top of the `--guard`
  mechanism).
- DT2 1.16 boot to ready and 40M further instructions, watch on `cf.expansion` (`0x47e8b000-0x47fe0000`) after
  the CACR write: 0 writes, 0 flush hits; the check saw 105.6M stores, 142K fetches and 1.03M CPUSHL. A control
  range in the display buffer (`0x44e4a5b8+0x400`) gave 691,512 writes from 18 PCs.
- When off, the store path tests one flag, as before; the work is in a cold function.
- Limits: all bytes of an instruction are checked; only DDR is tracked; host writes are not seen. The
  emulator does not map the uncached alias (`0x48000000+`) onto DDR, so a store through it lands in separate
  memory: code that relies on the alias would work on hardware and fail in the emulator **[O]**.

## S6: card sectors and a module store **[D]**

- Block functions, each taking the eMMC mutex inside: DT2 1.16 read `FUN_4012deda`, write `FUN_4012e0c0`, erase
  `FUN_4012ddf2`; DN2 1.11 read `FUN_4012c59a`, write `FUN_4012c780`, erase `FUN_4012c4b2`. Erase is CMD35/36/38
  with trim. Every call is bounded by the card capacity; buffers that are not 16-byte aligned or a multiple of
  512 go through a bounce buffer.
- Both devices: MmcFs header at sector 0, table at 0x800, project slot s at `0x58000 + s*0x8000` (127 ends at
  0x456259), project records at 0x40000 and 0x48000, the working project at 0x50000. DN2 sounds 0x1000-0x2000,
  kits 0x2000-0x11FD5; DT2 sounds 0x12000-0x16000, kits 0x16000-0x36000. MmcFs writes above 0x457fff are refused.
  DT2 also reads a factory table at 0x458000 and has the path filesystem from 0x5D8000 to 0x2DF0180.
- Highest sector in normal use: DN2 0x456259, DT2 0x2DF0180. Format trims only fixed regions; nothing depends on
  the card's size beyond the whitelist check.
- **[C] Superseded by finding 59 (2026-10-22):** the store moves to the areas above the +Drive (DT2) and above `0x800000` (DN2); the range below was reachable by a crafted Data-API request and too small for samples. What follows is the earlier proposal.
- Proposed module store: sectors 0x38000-0x3E000 (12 MiB) on both devices: above every kit pool, below the
  project records, outside every erase range, with 0x2000 sectors of margin on each side (a choice, not a
  measurement).
- The `MidiRpcFsRaw*` and `FsSample*` requests are not dispatched on DN2 (and FsRaw is not on DT2), so moving
  module files to a device needs a handler of our own later. The slot Data RPC is live on both.
- Open **[O]**: that nothing writes the store range during save/load, format, OS update and the Data RPC (a card
  write-watch run); the production UART console's reach.
