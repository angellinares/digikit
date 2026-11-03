# DN2 object of core/adapters/sample-pool

Status: accepted by Em on 2026-11-01 with the recommendations in section 7. Built in lane/sample-pool (ColdFire and DSP objects, allocator, transport, dsp-link chaining) and host-tested. The transport (sections 2.2, 2.3, 4 and 5) was rebuilt on 2026-11-02 as a job on the stock SampleLoader worker task, after the first emulator run showed 30 pages/s (one page per UI pass); section 8 has the static checks behind it. The DN2 emulator test of section 6 is still to do for the new transport.

Target: DN2 OS 1.12. Plug-in point: `modules/core/runtime/include/dkrt_pool.h` (version 1, unchanged except one additive symbol, section 2.6).
Sources read: AGENTS.md, docs/design/sample-api-1.5.md, findings 92, 95, 104, 106, README and sources of `core/dsp-lp0-proof`, `src/cf/dt2/pool.c`, `src/cf/pool_core.h`, `devices/fw/*.toml`, runtime `store.rs`, `tracks.rs`. Marks: [D] read in repo or on 1.12 bytes, [O] open or my estimate.

## 0. What exists

- `pool_core.h` is **not** device-neutral. It holds the DT2 stock slot model (`SP_SLOTS`, key words, record stride, `sp_geometry`, `sp_pick_slot`, `sp_free_frames`). Only `SP_NW32_BASE`, `SP_DDR_PHYS*`, `SP_NATIVE_RATE` and `sp_nw32()` are neutral. Plan: move those into a new `src/cf/pool_nw32.h`; `pool_core.h` includes it. No other change to the DT2 object.
- Profile `[adapter.sample_pool]` for DN2 already has `ddr_base = 0x80531000`, `ddr_bytes = 0x0ff80000` (256 MiB minus 512 KiB, so the range is `[0x80531000, 0x904b1000)`; the top 512 KiB of the former 256 MiB hold the R rings of the stereo machines, `ring_r_base` in the same section, docs/design/dn2-stereo.md). `patchkit gen` kind `gas-sample-pool` already writes them as `.equ SP_*` for ColdFire assembly. The DSP needs the same two numbers (section 2.7).
- The runtime already dedups by file identity and refcounts (design 2.1). The adapter still keeps its own block table, because it also keeps released blocks as a cache (1.4).
- DN2 runtime gaps this object depends on (not adapter code): `tracks.rs probe()` returns `E_CONTRACT` unless `device() == 1`, so `inspect/stat` need a DN2 branch that reads the store table (size and sha256 from the store entry, `hash_kind = DK_FILE_HASH_SHA256`); and the store has no range read (section 2.2).

## 1. Allocator (`src/cf/pool_alloc.h`, host-testable, no firmware access)

**Range.** `[ddr_base, ddr_base + ddr_bytes)` from the profile, never a constant in code. 4 KiB aligned (`0x80531000` is). The stock gap `[0x8052d790, 0x8052fbe0)` lies below `ddr_base`; add a build-time check that `ddr_base >= 0x80531000` (`0x80530be4` is the end of the last stock write, finding 106 canary map) and that the range ends at or below `0xa0000000`. 256 MiB stays a placeholder until the extended soak; the soak result goes into `docs/findings/`.

**Unit: 4 KiB, not 64 B.** Reasons:
1. The transport page is 4096 bytes (1,024 words) and the DSP receive buffer, copy loop and ACK all work per page. With a 4 KiB unit every page write is aligned and complete, so a page needs only a destination page index. No length field, no partial copy, no unaligned DSP copy. The last page of a plane is zero padded by the CF side.
2. A 64 B unit (cache-line size) would need a byte offset and length per page, and a partial-page path in the interrupt handler.
3. Cost of 4 KiB: on average 2 KiB lost per plane. For 256 MiB this is below 0.01 percent per sample at 100 samples. A 0.1 s mono hit (9.6 KB) rounds to 12 KiB, acceptable.
4. Index width: 65,536 units fit exactly 256 MiB. The page tag carries 20 bits so the range can grow to the 507 MiB soak limit and beyond without a protocol change.

**Structure.** Sorted free-extent list (start page, page count), fixed array of 66 entries (at most 64 blocks plus holes), coalescing on free, first fit. No bitmap (8 KiB, O(65536) scan) and no per-page metadata. Block table of 64 entries: `{first_page, pages, state, refs, token, frames, channels, lru, id (root,size,hash_kind,hash[32])}`.

**Stereo.** One block holds both planes: `plane_pages = ceil(2*frames / 4096)`; L at `first_page`, R at `first_page + plane_pages`; one allocation of `channels * plane_pages` pages. Mono: `base_r = base_l`. `base = sp_nw32(ddr_base + page*4096)`. Example: the first page gives `base_l = 0x1014C400`.

**Dedup and cache.** `dkpool_load` with an id whose `hash_kind` is valid (the DN2 store always gives SHA-256) first looks up a block with equal `(size, hash_kind, hash)` in state READY (refs++, the block keeps its token so that the first holder's handle stays valid, `DKPOOL_DONE` at once) or CACHED (refs = 1, state READY, new token, `DKPOOL_DONE`). A hit never touches the DSP. Without a valid hash there is no sharing. States: FREE, LOADING, READY (refs > 0), CACHED (refs = 0, content valid, evictable). `dkpool_release` decrements; at 0 the block becomes CACHED (not freed). When an allocation fails, evict CACHED blocks oldest first (coalesce after each) and retry; still no fit gives `DKPOOL_E_POOL`. This makes release/reselect reuse free of transfer cost. It is safe because the runtime closes and drains the track generation before `dkpool_release` (design 2.1), and content never changes for an identity.

**Tokens.** One u32 counter, +1 per `dkpool_load` call that returns a handle, from 1; at `0xffffffff` return `DKPOOL_E_CONTRACT` (never wrap), as the DT2 object does.

**free_frames.** The runtime wants mono planes. Return `largest contiguous run of (free + CACHED) pages * 2048`. Total free space would overpromise: a stereo sample needs one run for both planes, and PCM never moves, so no compaction exists.

**check / lost.** DN2 has no external reset of our range (finding 106: 20 scenarios, no stock write above `0x80531000`). `dkpool_check` returns 0 for a current READY handle (`id` = block index, token match) and `DKPOOL_E_LOST` for a stale one. A dead link (status 3) fails new loads with `DKPOOL_E_READ` but does not invalidate loaded samples: their PCM is still in DDR. `dkrt_pool_lost` is never called. [O] Reads and the 1,136 unresolved stores of finding 106 stay open: the hardware read-back later.

## 2. Transport

### 2.1 Reuse of `core/dsp-lp0-proof`

Path: CF reads from card, our own page sender (`send.c`, section 2.2: the byte order of the stock helper `0x400cf2dc`, which sends each word least significant byte first, so the file's big-endian PCM16 becomes the packed little-endian words the DSP kernels read and mono needs no CF conversion), SPI/LP0 into an armed 4,100-byte receive descriptor on the DSP, callback copies to DDR through the NW32 alias, rearms, ACK rides on the reply extension. Proven in the emulator for 235 pages in finding 95 (run b734a462).

Reused **verbatim** (move or include, no logic change):
- the boot window hook `_lp_boot_hook` .. `_lp_boot_restore` (RR4 noncacheable 4,352-byte window around descriptor, state, RX; anchors `dsp.cache.*`) and its status/error words;
- `lp_wire_size` (0xabc to 0xacc) and the DSP `boot_wire_size` write `hex = "840fcc0a"`;
- `lp_stock_copy` wrapper pattern `lp_copy_reply` (ISR, one writer, seqlock snapshot), the 16-byte extension framing;
- `_lp_open/_lp_set_callback/_lp_submit` setup order, `_lp_arm` descriptor words (`XCNT` 0x401, `0x00100000`), callback frame save/restore, local-interrupt mask across rearm, MODE1 restore.

Changed:
- Page tag: `0xD0000000 | seq8 << 20 | page_index20`. `page_index` is relative to `ddr_base`. The DSP derives the destination as `0x10000000 + (ddr_base - 0x80000000)/4 + page_index*1024` (words), checks `page_index < ddr_pages` against profile constants (a bad index writes nothing and reports error 4), copies always 1,024 words. The proof's sequential-tag and byte-count rule (tag must equal pages so far) goes away; any order is legal. `seq8` increases by 1 per page, mod 256; the DSP accepts an exact repeat of the last tag (idempotent copy, so a retry is safe) and rejects other seq values with error 5.
- ACK extension (8 numeric halves, `src/cf/pool_wire.h` has the format): magic `0x5350` and version 2 in halves 0 and 1; half 2 the status (0 unarmed, 1 armed idle, 2 armed after an error, 3 dead); half 3 the error code (low byte) and a count of rejected pages (high byte); halves 4 and 5 the accepted-page count since boot; half 6 the last accepted tag as `seq8 << 8 | page_index & 0xff`; half 7 a checksum of halves 0-6 (`sum mod 65536 xor 0x5a5a`), so a torn read fails closed. CF decodes with a seqlock snapshot like the proof's `lp_ack`. A page is acknowledged when the status is 1, the count is one more than before the send and half 6 names the page; an error is new when the error count differs from the one before the send (a stale error cannot end the next page). Before the first page of a load the receiver must report status 1 or 2 (armed), the rule that finding 92 requires. The next tag continues after the sequence number in half 6, so a load that gave up after a lost page starts right.
- Callback no longer takes `DK_SAMPLE_BANK_LOCK`, no reader publish, no 960,000-byte bank, no canary check (tests keep canaries host-side).

### 2.2 ColdFire range reader and conversion

The store file is contiguous sectors (`store::read_sectors(first, n, buf, cap)`), so a range read is sector arithmetic. Finding 104's `STAGE_BYTES` (64 KiB, whole file with sha check) cannot hold a long sample. Add to the runtime, additive, C ABI in `dkrt.h`:
- `int32_t dkrt_store_stat(const uint8_t *path, dkrt_store_file *out)`: `{first_sector, size, sha[32]}`. Uses `find()`; leaves the staging buffer dirty, so call only at load start.
- `int32_t dkrt_store_read(const dkrt_store_file *f, uint32_t sector, uint32_t count, uint8_t *dst)`: no staging buffer, no sha, bounds checked against `f->size`.
`dkrt_store_stat` runs in the UI loop task; `dkrt_store_read` also runs in the worker task (section 8.2).

Adapter buffers: static `.bss` in the adapter object, one 64 KiB read buffer (128 sectors, the most `dkrt_store_read` takes) and two 4,096-byte output pages (the converted chunk), 72 KiB in all. Do not borrow `DKRT_STAGE`. One read covers whole chunks: 15 mono or 7 stereo (`spw_span_of`). Chunk j of a read is at byte `64 + j * chunk_bytes` of the buffer, because a chunk is a multiple of 512 bytes.

**Page sender (`src/cf/dn2/send.c`).** It replaces the stock sender `0x400cfb0e` (anchor `cf.link.fn.bulk_send`, no longer used by this module). The stock sender locks a mutex (`0x42441964`), sends the tag and 1,024 words, then sleeps 100 units on a timer. The only code that touches the port is the word helper `0x400cf2dc` (the tag always, the data when the sender's format argument is nonzero; argument 0 selects `0x400cf368`, which sends b1 b0 b3 b2 and is not what the DSP expects). Our loop does the same per word: write 0x0080 to `0x8c00000a`, poll bit 0 of the half at `0x8c000002`, then write 8 halves to `0x8c000002`: for the bytes b3, b2, b1, b0 of the word as it lies in memory, `byte << 8 | 0x80` and then `byte << 8`. The mutex protects nothing else on DN2 (no other code writes the port), the 100-unit wait is replaced by the acknowledgement, and the ready poll is bounded: 8,192 reads of one word, then the page stops and `sp_raw_send` returns -1 (the load counts it in `sp_stuck_sends`; the page is lost and the acknowledgement timeout sends it again). The port addresses are constants in `send.c`; the host test replaces the three access functions with a model of the port. Cost per page: 1,025 words with 9 port accesses each; 0.29 to 0.89 ms depending on the wait states of the external bus (3 to 20 CPU cycles per access, 264 MHz) [O, unmeasured]; one frame is 0.667 ms.

Per **mono** page k: file bytes `[64 + 4096k, 64 + 4096(k+1))`; the page is the chunk's bytes copied (the tail of the last page is zero). Per **stereo** chunk c (2,048 frames, source `[64 + 8192c, +8192)`, sectors from `16c`): deinterleave halfwords into an L page and an R page (file order L R L R, planar output), then send L page `L_first + c`, R page `R_first + c`. Halfword order stays big-endian; the sender's byte order treats both planes like mono. Last page of each plane: zero from the PCM end to the page end (the trailer and bytes beyond are not PCM). CF cost for deinterleave is about 4,096 halfword moves per chunk [O: CF clock].

The header (64 bytes, sector 0) is parsed by the adapter again with `dkrt_sdk::sample::native_header` semantics (channels 1 or 2, 48 kHz, PCM16, `size == 64 + P + 16`); mismatch gives `DKPOOL_E_FORMAT`. Full-file sha256 verification is **off** in v1: the store verified the file at upload, the runtime checks the table sha against its snapshot, and CF sha256 over 5.76 MB would add an estimated 0.3 to 0.6 s [O] to a 30 s load. Option flag later.

### 2.3 The job, the UI tick and the acknowledgement

One transfer at a time (one link). `dkpool_load` (UI task) allocates the block, parses the header, returns `DKPOOL_QUEUED` (or `DONE` on a cache hit) and keeps the request in a queue of 16. `dkpool_tick` (UI task, once per UI pass) does two things only: when a job has ended it delivers the result (`finish`: allocator calls, `dkrt_pool_done`; never from the job or the interrupt), and when no job is active it starts the oldest queued request: it fills the job record and queues a job on the SampleLoader worker (`cf.samples.fn.worker.queue`, the same std::function layout as the DT2 adapter). `job.state` is the hand-over: the UI task sets RUN last, the job sets DONE last, the UI task sets IDLE after `finish`. A cancelled request that has not started is finished by the tick without a job.

The job runs in the worker task (`run_job`): it reads 64 KiB, converts a chunk into one page (mono) or two (stereo), and for each page: waits until the receiver is armed (before the first page), sends the page, then looks at the acknowledgement snapshot every 100 us (`cf.rtos.fn.sleep_us(100)`, section 8.2) until the page is acknowledged. The acknowledgement path is unchanged: the hook `sp_copy_reply` decodes the reply extension in the interrupt into a seqlock snapshot, and `spw_poll` (pool_wire.h) judges it. Bounds: armed wait 300 blocks (200 ms); acknowledgement 75 blocks (50 ms) per attempt with 3 resends of the same page; and a count of 6,000 sleeps per wait, which ends the wait when the block clock does not run (`dkrt_blocks` is 0 without core/health). A reply with status 2 (DSP error) ends only the current load (`E_FORMAT` for error 4 or 5, `E_READ` otherwise); the link stays armed. Status 3 (rearm failed) marks the link dead: the load ends with `E_READ`, and all later loads fail with `DKPOOL_E_READ`; the runtime diagnostics record it; recovery needs a boot.

Cancel and supersede: `dkpool_cancel` sets `cancelled` (a volatile word) in the request; the job reads it before a card read and before each page. A page in flight is always settled first (acknowledged or failed), so the receiver's counters are known when the next load starts; then the job ends with `E_LOST`. A superseding `dkpool_load` only queues. A cancelled job's block is freed by `finish`, which also reports `E_LOST` once.

The job touches only its own buffers and record, the acknowledgement snapshot, the card read and the port. The allocator, `dkrt_pool_done` and `dkrt_store_stat` stay in the UI task.

Fallback, not built: the page send in the block interrupt (`sp_copy_reply` sends the next page after decoding an acknowledgement; ring of 32 pages; the job's role reduced to card reads). It reaches the frame rate (about 1,500 pages/s with a gap of two frames) but puts 0.29 to 0.89 ms of port writes into every other frame interrupt, which has no precedent in the stock firmware (stock sends pages from tasks) and may delay the ESDHC and USB interrupt handlers. Build it only if the worker job proves too slow on hardware.

### 2.4 Abort and supersede

Built: `dkpool_cancel` and `dkrt_store_stat` / `dkrt_store_read` exist with the final signatures in sample-api-1.5.md section 6 (the runtime calls cancel when no track waits for a load; the DN2 probe works). The text below is the design they follow.

`dkpool_load` has no cancel today and the design says the old transfer finishes and its result is dropped. For 30 s samples that is up to 1 to 3 s of wasted link time. Recommend an additive optional symbol `void dkpool_cancel(uint32_t req)`: marks the load cancelled; the in-flight page finishes (it cannot be recalled), no further page is read or sent, the block is freed, and the adapter calls `dkrt_pool_done(req, DKPOOL_E_LOST)` once (the runtime drops a stale req). Old content in a freed block is harmless: nothing reads an unpublished block.

### 2.5 Coexistence

- Normal audio: pages are sent from the worker task, copied by the LP0 callback in interrupt context (about 1,030 cycles for 1,024 words plus cache fill, at most 1,500 pages/s). The DSP only writes blocks that no voice reads (LOADING or evicted-CACHED, both unreferenced).
- `core/dsp-link`: both claim `dsp.link.site.block_tail` on the DSP today (that is why the proof conflicts). Fix: dsp-link's DN2 tail calls an optional extern `_sp_tail` (added to its `optional` list like `_dl_vrows`; 0 when no pool). The pool does its per-block work (first-block open, reply extension write) there. No second claim. The CF side needs no change: proof ops use `cf.link.hook.reply_copy` and `cf.link.hook.boot_wire_size`; dsp-link uses `cf.link.hook.block_isr.frame_call` and the literal `reply_copy_lit` (different sites, claims must be checked by `patchkit` once).
- `core/runtime`: owns `cf.view.hook.ui_loop` and calls `dkpool_tick`; the proof's `tramp.s` and its own UI hook are dropped.

### 2.6 Drop from the proof

`lp_sampler` machine and `DK_PARAM/PAGE`; `readers.s` (A/B diagnostic readers, snapshots, `_lp_close_*` records); the fixed 960,000-byte bank at `0x80800000` and the 1 MiB-style canary guard loops; the CLOSE tag (`0x01014243`) and its mailbox/ACK kind; `lp_mail`/`lp_close_mail` host mailboxes (the external test driver); the 65,536-poll CLOSE budget; "errors need a fresh boot" for data errors; module `requires` (`core/machines`, `core/params`, `core/dsp-voice`) and `conflicts` (`core/dsp-link`, `core/runtime`, `core/dsp-ddr-proof`); `_sp_status`, `_sp_error`, `_sp_errcount`, `_sp_pages` and `_sp_last` are the small test oracles (the proof's `_lp_bytes` is gone: pages are counted, not bytes). The hard assumption "private pre-init cold-idle fixture" is no longer manual: finding 95 records that the harness now initializes changed DSP code from raw `cold.state`.

### 2.7 DSP range constants

New gen output for SHARC: `SP_DDR_BASE_NW32` and `SP_DDR_PAGES` (from the same profile fields). Extend `devgen.rs` kind `sharc` (or add `sharc-sample-pool`); one source of truth.

## 3. Coherence

Checked on DN2 1.12 bytes with `fw.py disasm 0x1c07e4 -i dn2-1.12 --cpu sharc`: the helper entry is **0x1c07e4** (0x1c07e3 is the preceding reserved parcel). R4 start and R8 end are read at entry (`I12 = R4`, `I14 = R8`); R12 goes to R14 as mode. It is a framed function (saves R10, R11, R13, R14, R15, I5, I14, I15, I12 on I6/I7 frames, `RETURN` at `0x1c08fe`), reads CFG (`0x3e000`) and exits early when bit 8 is clear, assumes the startup modifier constants M5=0, M6=1, M7=-1, M9, M13, runs a hardware `DO ... UNTIL LCE` loop per 64 B line, clears and restores MODE1 IRPTEN (bit 12) around each line, calls `0x1c0214`, `0x1c0241` and `0x1c0903`/`0x1c002a` for address conversion, and rewrites range 0 and CFG2 without restoring RR0. Mode zero is writeback only, nonzero writeback plus invalidate (finding 95, matches the code shape). Callable from the receive callback: yes in principle, because the callback's own rearm (`_lp_submit`) reaches the same helper through the stock nested submit (finding 95, `0xb8e259`; I did not re-trace that hop). But the address-conversion forms (`0x1c0807`, `0x1c081b`) are the unresolved Type 7a encoding of finding 95: the emulator cannot show whether DDR ranges are converted correctly, and a post-DMA writeback can overwrite newer data. Not provable here.

**Recommendation: neither in v1.** Reasons:
- DDR in our range has one user, the SHARC core, which writes pages through the NW32 alias and reads PCM through the same alias and the same cache. A single cache is coherent with itself, so no writeback is needed for audio. Dirty lines are written back later by normal eviction; nothing else writes this range.
- RR4 as a noncacheable DDR window is the wrong tool: RR4 is already the RX window (one range pair per selector, `0x604`), and making PCM noncacheable puts every sample read on the DDR path.
- Fallback if the hardware read-back shows stale data: one writeback plus invalidate per **load** (whole block, mode 1) from the block tail, outside the interrupt handler and with range-0 ownership, never per page. Needs a new anchor use only (`dsp.cache.fn`-style entry for `0x1c07e4`, rule `sharc_func`), and an encoding proof first.
Keep RR4 for the L2 RX window as proven.

## 4. Load-time ceiling

Pages (4 KiB each, both planes): stereo 5 s = 960,000 B = 236 pages (118 per plane); stereo 30 s = 5,760,000 B = 1,408 pages (704 per plane).

The first transport sent one page per UI pass (about 30 per second, measured: 376 pages in 18,636 blocks). The job does not wait for the UI pass. Its rate is set by the send time and the wait for the next reply: a page is sent, the DSP copies it in the LP0 callback and writes the acknowledgement into the reply of a later block (latency L frames, 1 to 3 [O]), and the job looks every 100 us. The host model (`tests/patchkit/test_sample_pool_dn2.py::test_page_rate`) gives, in pages per second for a 3 s stereo load:

| send time per page | L = 1 | L = 2 | L = 3 |
| --- | --- | --- | --- |
| 0.30 ms | 1,166 | 656 | 456 |
| 0.89 ms | 656 | 457 | 350 |

The rate is the same with a UI pass every 3 ms or every 267 ms. The card read (one multi-sector read per 7 or 15 chunks, about 2.5 ms for 64 KiB at 26 MB/s [O]) is 14 or 15 pages' worth of data per read, so it adds about 0.2 ms per page to the slow case. Stereo 5 s takes 0.2 to 0.7 s and stereo 30 s 1.2 to 4 s by this model. The floor of the emulator test stays 200 pages/s. For comparison, `core/dsp-link` is 72 KB/s on DN2 (finding 104): 80 s for 30 s stereo. [O] Unknown until hardware: the port wait states (the send time), L, the card read time, the LP0 link clock. Pipelining two pages needs a second RX descriptor (a chained descriptor changes the STOP ownership result of finding 95): not in v1.

## 5. Threading

Three contexts, one writer per field. UI-loop task: `dkpool_load/release/check/free_frames/cancel`, `dkpool_tick`, the allocator, the block table, `dkrt_pool_done`, `dkrt_store_stat`. SampleLoader worker task: `run_job` (card reads with `dkrt_store_read`, conversion, the raw send, the sleeps, `spw_x`). Interrupt: `sp_copy_reply` writes the acknowledgement snapshot only. Shared words are `job.state` and `job.result` (job.state is the hand-over), `cancelled` (UI writes, job reads) and the snapshot (interrupt writes, job reads with the seqlock). Every shared word is a volatile u32. The worker is the stock task of the stock Digitakt II loader; the job runs there because that task already exists, has a queue, and the stock firmware sends its pages from it. DSP side: the LP0 callback is an SEC interrupt handler; init and rearm-recovery run from the block tail (AudioTask) as the proof does.

## 6. Files, anchors, tests

Add:
- `modules/core/adapters/sample-pool/src/cf/pool_nw32.h`, `src/cf/pool_alloc.h`
- `src/cf/pool_wire.h` (page tag, ACK format, page state machine, header, chunk and read geometry, deinterleave), `src/cf/dn2/xfer.h`, `src/cf/dn2/pool.c` (plug-in symbols), `src/cf/dn2/xfer.c` (the tick, the worker job, the two link hooks `sp_wire_size` and `sp_copy_reply`, written in C: no hook.s), `src/cf/dn2/send.c` (the raw page sender), `src/cf/dn2/device.s` (range from the profile equ file)
- `src/dsp/dn2/boot-window.s` (verbatim split), `src/dsp/dn2/sample-pool.s` (receiver, callback, reply, `_sp_tail`)
- `module.toml`: `ports += "dn2-1.12"`, `[targets] "dn2-1.12"`, two `[[object]]` (cf `devices=["dn2"]`, dsp `place="dsp.l2"`), externs `cf.link.fn.reply_copy`, `cf.rtos.fn.sleep_us`, `cf.samples.fn.worker.get`, `cf.samples.fn.worker.queue` (CF) and as the proof ( `dsp.cache.fn.cache_range`, `dsp.link.fn.lp0_*`, `dsp.link.value.lp0_record`, `hook.dsp.cache.site.cache_enable.return` (DSP)), `[[op]]` hooks `cf.link.hook.reply_copy`, `cf.link.hook.boot_wire_size`, `dsp.cache.hook.cache_enable`, write `dsp.link.site.boot_wire_size`, claims as the proof except `dsp.link.site.block_tail` and `cf.view.site.ui_loop`.
Change:
- `modules/core/adapters/dsp-link/module.toml` and `src/dsp/dn2.s`: optional `_sp_tail` call in the tail.
- `modules/core/runtime`: `dkrt.h`, `store.rs` (stat and range read), `tracks.rs` (DN2 `probe`; optional `dkpool_cancel`), `dkrt_pool.h` (additive `dkpool_cancel`).
- `devices/digitone-ii.toml` (comment: object now exists), `packages/patchkit/src/devgen.rs` (SHARC equ).
- Anchors (`devices/fw/link.toml`, `devices/fw/dsp/link.toml`, `devices/fw/dsp/cache.toml`): add `"core/sample-pool"` to `users` of the existing entries: `cf.link.fn.reply_copy`, `cf.link.site.reply_copy`, `cf.link.hook.reply_copy`, `cf.link.site.boot_wire_size`, `cf.link.hook.boot_wire_size`, `dsp.cache.fn.cache_range`, `dsp.cache.site.cache_enable`, `dsp.cache.hook.cache_enable`, `dsp.link.fn.lp0_open`, `lp0_set_callback`, `lp0_submit`, `dsp.link.value.lp0_record`, `dsp.link.site.boot_wire_size`. Rule kinds stay `cf_func`, `sharc_func` (reloc_hash), `sharc_literal`. `cf.samples.fn.worker.get` and `cf.samples.fn.worker.queue` (declared for DT2) now list DN2 as well, and `cf.rtos.fn.sleep_us` is new (DN2, `devices/fw/rtos.toml`); all three are `cf_func` rules that resolve on 1.12. `cf.link.fn.bulk_send` is used by `core/dsp-lp0-proof` only. A `sharc_func` anchor for `0x1c07e4` only if the fallback in section 3 is needed. DN2 1.11 port: not in scope; the hashes are 1.12 only.
- Retire `core/dsp-lp0-proof` only after the DN2 test passes; its boot fixture test stays until then.

Tests:
- Host (`tests/patchkit/test_sample_pool_dn2.py` compiles `pool_alloc.h`, `pool_wire.h` and the real `pool.c`, `xfer.c` and `send.c` against `tests/fixtures/sample-pool-dn2/harness.c`, which runs the block interrupt, the UI task and the worker job on one simulated clock; `test_sample_pool_dsp_dn2.py` runs the assembled receiver against stand-ins of the stock driver): first fit and coalescing; stereo R follows L and plane arithmetic; range and 66-extent bounds; CACHED eviction order; dedup refcount and token uniqueness (no wrap); `free_frames` as largest run; `sp_nw32(0x80531000) == 0x1014C400`; page tag encode/decode and seq rule; stereo deinterleave and last-page zero padding against a Python reference; ACK decode torn-read cases (reuse the stand-in harness of `test_dsp_lp0_proof.py`).
- One DN2 emulator test (a new Vitest file `sample-pool-dn2.test.ts` in `tests/regression/runtime`, bounded by `--seconds`, existing ready-pair cache): upload one stereo file (about 3 s) and one mono file to the store, select on two tracks, assert:
  1. status goes QUEUED, then READY, with the right token; `dk_strack` words (frames, channels, `base_l`, `base_r`) match;
  2. DDR planes read back byte-equal to the file's L and R planes (via the daemon DDR peek), and pad bytes after the last frame are zero;
  3. dedup: the same file on a second track gives equal bases and no new pages (page counter unchanged);
  4. release then reselect: the second select returns READY without transfer (counter unchanged), content unchanged;
  5. supersede mid-load: start a long file on track 2, select another file on track 2 after about 20 pages; a tone playing on track 1 stays finite and non-silent through the whole window (the proof's held-audio check); the old block is freed (free_frames returns);
  6. pages per second = accepted-page counter delta over the DSP frame counter delta; report, and fail only below a floor of 200 pages/s. Also report: the blocks per page, `sp_stuck_sends` (must be 0) and that the worker job ran at all (the first load creates the worker task).
  Failure cases in the same file only if cheap: a bad path gives `NOT_FOUND`; a pool of too small size is covered on the host.

## 7. Open questions for Em

1. ACK channel and the wire-size change (0xabc to 0xacc, all DN2 SPI2 descriptors, boot only). Recommend: keep it for the emulator-first v1 (proven), and before any hardware step decide whether to move the ACK into spare words of the dsp-link diagnostic record (no wire change). Finding 88 says exclusive ownership and hardware safety are unproved.
2. Add `dkpool_cancel` (additive)? Recommend yes; without it a superseded 30 s load holds the link 1 to 3 s.
3. Keep released blocks as a cache? Recommend yes (cheap, makes project switching and reselect free); evict LRU on pressure.
4. Verify sha256 of the streamed file? Recommend no in v1 (store already verifies at upload; costs 0.3 to 0.6 s per 30 s sample); revisit after hardware.
5. Coherence: no maintenance in v1, per-load helper call as fallback, only after a hardware read-back shows staleness and the 7a encoding question is closed. Recommend this order.
6. DN2 range size: 256 MiB now; note a 30 s stereo sample is 5.8 MB, 256 MiB holds about 23 min stereo. Recommend keep and raise only with the soak.
7. Who owns the DN2 `probe` and the store range read (runtime change, small but outside the adapter)? Recommend do it in the same lane, as the adapter has no use without it.

## 8. Static checks behind the worker job (2026-11-02, DN2 1.12, one reader)

### 8.1 Does the stock FM Drum use PCM that overlaps our range?

Question: the FM Drum engine has a transient, and the DN2 SampleLoader worker might exist to load PCM for it. Result: **no evidence of that, and none of an overlap with `[0x80531000, +256 MiB)`** [D, bounded].
- DN2 has no sample-loading code to match the worker: the strings `SampleLoaderBgWorker` exist only as RTTI names (no direct users), there is no "SAMPLE POOL" string, and the DT2 loader's functions have no DN2 pair (finding 90 names the worker, not a producer). The worker singleton accessor `0x4019eb1a` has one caller, `0x4005e28a` (a status or Loading-toast path); the entry `0x400f28e6` is referenced only by the constructor `0x400f2870` (data reference at `0x400f2884`). The accessor builds the singleton on its first call (`tst.l 0x44648a20; bne` then construct), so on DN2 stock the worker task may never exist unless that path runs [O: whether it runs at boot was not traced]. Our first load creates it.
- The FM Drum voice on the SHARC (`0x1c778b`, finding 53; callees `0x1c8832`, `0xb81368`, `0x1c06ba`, continuation `0x1c79cb`) holds no literal address in DDR: its tables are at `0x2673a8`, `0x267428` and `0x266ce0` (internal memory, below `0x300000`), and its jump table is internal. The "Drum Transient" and "Transient Level" strings are CF parameter names with no users in code. So the transient is computed or read from internal tables, not loaded from the card.
- Limits: a literal scan of five functions, not a closure over the pointer tables, so a DDR access through a computed pointer is not excluded. Finding 106's canary soak never selected FM DRUM, so the soak does not prove the engine leaves our range alone. One FM DRUM selection in the next soak closes that for little cost.
- Nothing to reuse: there is no stock FM Drum transport or pacing on DN2. The only stock model for a task-sent page stream is the Digitakt II loader (finding 109 and the comparison below), which the job follows.

### 8.2 The job in the worker task

- Worker: constructor `0x400f2870` passes, among other words, `0x4000`, `3`, the entry and two statics to the task base constructor `0x400f22b4` (read as a stack size of 16 KiB and a priority of 3 [O: the argument roles were not traced]). Singleton `0x44648a20`; enqueuer `0x400f25f8(worker, name, job, done)` (the same signature the DT2 adapter uses, same rule hash on both devices); loop `0x400f2498`. The job runs as a std::function invoker: the layout is the DT2 adapter's (storage, manager, invoker).
- `dkrt_store_read` in the worker: it computes a sector span from its arguments (`file_span`), then calls the firmware's `block_read` (`dkfw::storage::block_read`). It uses no staging buffer and no shared state beyond `init_once` (idempotent after boot) and `card::state()` (a read). The firmware block read takes the ESDHC mutex and waits on a semaphore (finding 36), as every task that reads the card does (project saves run on the stock BgWorker the same way). The runtime holds no lock around it. The comment "run in the task that owns the card reads" in `store.rs` is a convention: a concurrent read by the UI task (a module load or upload) is serialised by the ESDHC mutex. `dkrt_store_stat` stays in the UI task (it uses the staging buffer).
- Sleep: stock `0x4013492c(us)` (anchor `cf.rtos.fn.sleep_us`; DT2 `0x40136544`) is a general short sleep with eight stock callers. It locks its own mutex (`0x4463fc54`), loads timer DTIM1 (`0xfc074000`: mode `0x841b`, prescaler 0x84, about 1 MHz; `0xfc074004`: the count) and blocks the task on a semaphore (`0x4463fc4c`) until the timer interrupt. The first call installs the interrupt handler and the timer setup (`0x4463fc60` is the set-up flag, taken with SR at 0x2700). So `sleep(100)` is 100 us, task context only; the stock LP0 sender calls it after each page; stock DT2 pages out of its worker with the same call.
- Other tasks sharing the mutex wait up to the sleep (100 us); nothing in an interrupt calls it.
- Stock DT2 for comparison (comparison note B, 2026-11-02): its loader reads 32 KiB, sends 8 pages from the worker, sleeps 100 us after each page, has no acknowledgement and no interrupt send. Our job adds the acknowledgement (our DSP copies in the LP0 callback once per block) and reads 64 KiB.

## Estimate

Agent time: allocator plus host tests 3 to 4 h; DSP receiver change from the proof 4 to 6 h; CF transfer state machine, deinterleave, runtime store range read, DN2 probe 6 to 8 h; dsp-link chaining 2 h; emulator test and debugging 6 to 10 h. Total about 1.5 to 2.5 days. Outcome risk: emulator acceptance low to medium (the transport is proven; new work is pacing, claims and the DN2 runtime path); hardware outcome unknown and not claimed (wire change, coherence, stock sender with no timeout, throughput all [O]). Payoff is high: it is the only DN2 route to samples longer than 8,192 frames.
