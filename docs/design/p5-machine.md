# P5 design: native/machine (ColdFire + periph + SHARC + live audio)

Base: `5396f9a`. Sources read: `docs/plan-native-emulator.md`, `native/coldfire/src/{lib,cpu}.rs`
(Bus trait, `Cpu::step`), `native/periph/src/{lib,machine,dspi,ssi,spilink,dsp}.rs` (signatures),
`scratchpad/p4a-periph-timers.md`, `p4b-periph-dma.md`, `native/sharc/src/frames.rs`,
`native/live/src/{lib,sharc_source,producer,repeater,source}.rs` (signatures),
`emu/longrun.py` (`IdleSpin` 741-808, `spin` 999-1132, service order 1195-1207),
`emu/pit.py` (`interrupt_level` 170, `deliver_pending` 473), `emu/harness.py` (`raise_vector` 382),
`emu/livesharc.py` (`FrameForcer` 106), `emu/dspi2.py` (model and lockstep docs 60-190, `service` 464),
`emu/snapshot.py` (`save` 319), `tools/snapeq.py` (docstring), `docs/findings/04-coldfire-dsp-link.md` 12-31.

## 0. Two modes, one loop

The machine has two interrupt/idle policies behind one stepping loop:

- **`Mode::Oracle`** reproduces the Python machine exactly (`emu.longrun.spin` in pits mode, exact,
  not `fast`): interrupts are offered only at service boundaries, with the Python HLEs (IdleSpin
  vector 32, UART8 wait-loop delivery, FrameForcer, forced MMIO reads, `raise_vector`'s
  handler==0 / >=0x48000000 refusal). This is the only mode that can match `tools/snapeq.py`.
- **`Mode::Device`** is the hardware: the INTC's IPR/IMR/ICR/INTFRC (`IntcBank::hw_*`) are live,
  the CPU takes the highest unmasked level > IPL at the next instruction boundary, STOP idles to
  the next deadline, no FrameForcer (the firmware's own SSI0 -> vector 170 -> INTFRCH1[31] ->
  vector 191 chain, findings/04 lines 18-30). It is reached only after Oracle parity, and checked
  against Oracle-mode outputs (frame contents, UI events), not against snapeq.

Only exact stepping exists natively. Python's `fast=True` existed to avoid Unicorn's counted
`emu_start` tax (longrun.py 1057-1068); the Rust interpreter counts instructions for free.

## 1. Stepping loop

The **only time base is the guest instruction clock** `clock: u64` (= Python `pits.now` = `base+done`).
Guest seconds = `clock / ips` (`ips` from the profile, overridden by the snapshot's timer component).
No wall-clock read anywhere in the machine crate.

`Machine::run(instrs)` has `spin`'s contract: `instrs` is a floor; the loop finishes the deadline step
it is in, so one call of N and ten of N/10 execute the same stream (longrun.py 1017-1022).

```
while done < instrs:
    step = sched.next_step(clock)          # min of: Timers::step, Ssi0Dma::step, forcer (Oracle),
                                           # input queue, host budget (never subdivides a timer step)
    skipped = idle.skip(cpu.pc, step)      # §1.2
    ran = cpu.run(bus, step - skipped, hooks)   # stops early only on Halt/Unimplemented/unhandled
    clock += skipped + ran
    service(clock)                         # §1.1; then re-read PC (raise_vector moves it)
```

### 1.1 Service order at a boundary (Oracle; longrun.py 1115-1125, 1203-1205)

1. async events in list order: `Dspi2Link::service` (queued completion vectors 148/149, off by
   evidence), `Ssi0Dma::service` (one request if due; RX/TX minors against RAM; vector 170 owed),
   then the SHARC peer (none natively: see §3).
2. `Timers::service(clock)`: PIT then DTIM, each offering pending channels in `channels` order
   (PIT `(3,2,0)` after `release_intro`), each delivery gated by `level_for_vector` (ICR!=0, IMR
   clear), `IPL < level`, and handler present. A refused tick **stays pending** (held) and is
   offered again only at the next boundary (never at RTE). One tick per service, no backlog.
3. `on_chunk` equivalents: FrameForcer (Oracle only, when its period is due and IPL < its level),
   host input events (§5).

Delivery = `Cpu::take_interrupt(bus, vector, level)`: 8-byte frame `0x4000|vec<<2, SR, PC`,
live SR gets S set, T clear, IPL = level (level `None` = vector 32 from IdleSpin: IPL unchanged).

Speed: at 96 kHz SSI0 requests a boundary comes every ~1.4k instructions, so `service` must cost
tens of ns. Each source keeps its next deadline cached plus a `dirty` bit set by its register
writes; a source with `clock < next && !dirty` is skipped. Proof obligation: a test that runs the
same window with the short-circuit off and compares the export hash.

### 1.2 Idle skip

`idle.sites` = the idle loop's `bra.b *` PCs (`db.find_idle_spins`, resolved per image at load time).
Oracle semantics are `IdleSpin` exactly: per pass `count += 1`, UART8 TX `deliver()`, every
`every`-th pass raises vector 32 (run for real, not skipped); `skip` refuses while a TX completion is
queued. Natively the skip is taken **whenever PC enters a site**, not only at a step start: the
interpreter knows the exact count, so this equals running the passes (the Python restriction came
from Unicorn not reporting a stopped count, longrun.py 766-771). Device mode keeps the same skip
(the loop is still `bra.b *`); STOP additionally jumps the clock to the next deadline or pending IRQ.

### 1.3 PC hooks

A bitmap over the code region (1 bit per 2 bytes, ~256 KiB for the MAIN OS span), checked in
`Cpu::run` before each instruction. Sites: idle loop (per-pass `on_spin` when not skipped), UART8 TX
wait loop (vector 155 delivery), `flash_read` HLE (Oracle), SSI0 ISR RTE handover of vector 191
(Oracle, `emu/ssi.py`). Device mode clears all but idle and flash.

### 1.4 CPU additions (native/coldfire, small)

```rust
pub enum RunExit { Budget, Hook(u32), Irq(u8), Stop(Stop) }
pub trait Pins { fn pending_level(&self) -> u8; }          // Device: INTC; Oracle: always 0
impl Cpu {
    pub fn run<B: Bus, P: Pins>(&mut self, bus: &mut B, pins: &P, n: u64, hooks: &PcBitmap) -> (u64, RunExit);
    pub fn take_interrupt<B: Bus>(&mut self, bus: &mut B, vector: u8, level: Option<u8>) -> Result<(), Stop>;
    pub fn ipl(&self) -> u8;
}
```

## 2. The bus (`Board`)

One struct owns RAM and all peripheral models, so DMA effects run inside the bus call, exactly
where Python's write hooks run them.

Memory map (bases fixed by the MCF5441x; sizes from the profile, cross-checked against a snapshot's
`all_mapped`, 1 MiB pages = `emu.harness.PAGE`):

| range | region |
|---|---|
| `0x0000_0000..` | boot flash / FlexBus CS0, read-only (profile size) |
| `0x4000_0000..` | DDR (VBR = 0x4000_0000; MAIN OS at +0x400) |
| `0x8000_0000..` | internal SRAM (DSPI2 staging buffers, e.g. `0x8000_5348`) |
| `0x8C00_0000..` | DSP FIFO (FlexBus): `dsp::Fifo` status, data writes -> `SharcLink::flexbus` |
| `0xEC00_0000..0xFFFF_FFFF` | on-chip peripherals, 16 KiB slots |

Dispatch, two levels, no `dyn`:
- `pages: [Region; 4096]` (1 MiB): `Ram { off }`, `Rom { off }`, `Fifo`, `Io`, `Unmapped`.
  RAM/ROM reads and fetches are a bounds-checked slice access after one table load; `fetch16`
  keeps a last-page cache.
- `Io` pages index `slots: [DevId; 0x5000]` (16 KiB slots over `0xEC00_0000..`), `DevId: u8` enum,
  dispatched by `match` to `Timers`, `DmaLink`, `Ssi0`, eSDHC, GPIO, UART, panel, display.
  Unclaimed slots are `RegFile` plain RAM (the oracle never intercepts them).
- Oracle mode maps unmapped pages on first touch as zero RAM and records them in `mapped`
  (Python `Machine.ensure`/`_fault`), so the exported `all_mapped` matches.

DMA side effects: `DmaLink::write` returns `SerqEffect`; the Board completes it immediately with a
RAM slice (`finish_dspi2_capture(&ram[saddr..])`, `finish_tx35_capture`), applies `DeliverWrite`
to RAM, and hands TX bytes to the link. SSI0 minors read and write RAM at service time. Vectors the
effects raise are queued in `Sched` (Oracle) or asserted in IPR (Device), never taken inside a bus
call.

```rust
pub struct Board { ram: Ram, pages: [Region; 4096], slots: Box<[DevId; 0x5000]>,
                   pub timers: periph::Timers, pub dma: periph::DmaLink, pub ssi0: periph::ssi::Ssi0Dma,
                   pub fifo: periph::dsp::Fifo, /* esdhc, gpio, uart, panel, display */
                   pub events: EventQueue, /* raised vectors, link traffic */ }
impl coldfire::Bus for Board { /* read8..write32, fetch16 */ }
```

The `link` is not inside `Board` (keeps `Board: Bus` free of the host); the Board pushes link traffic
into `events` and `Machine` drains it to the `SharcLink` right after the instruction that caused it
(same guest clock, same order).

## 3. SHARC link

```rust
pub trait SharcLink {
    /// One DSPI2 full-duplex exchange, TX in wire order; fill `rx` (same length).
    fn exchange(&mut self, clock: u64, tx: &[u8], rx: &mut [u8]);
    /// Bytes written to the DSP FIFO data port, in order (boot and sample loads).
    fn flexbus(&mut self, clock: u64, addr: u32, bytes: &[u8]);
    fn ssi_rx(&mut self, _clock: u64, out: &mut [u8]) { out.fill(0) }
    fn ssi_tx(&mut self, _clock: u64, _data: &[u8]) {}
}
```

Implementations: `ZeroLink` (= `ZeroPeer`, parity runs), `RecordLink` (TX frames + FlexBus bytes with
clocks, in `.dt2cap`/`flexbus.raw` form for byte comparison), `QueueLink` (desktop, in native/live:
TX -> `FrameQueue::write`, RX zeros), `InlineLink` (renders each frame with `SharcRenderer` on the
calling thread; tests, single-worker wasm, and the future reply path).

Decision: **the SHARC runs on the existing producer thread, fed by the ordered `FrameQueue`, and the
ColdFire gets zero replies** (what every capture recorded and the trig-to-voice path was verified
with). Nothing flows from SHARC to ColdFire, so the guest is deterministic regardless of threads.
If a reply ever matters, `InlineLink` answers frame n with frame n-1's output at a fixed one-frame
guest latency, which stays deterministic.

Pacing: the host edge compares `machine.guest_time()` with audio played (producer frames x 667 us)
and parks the ColdFire thread when the guest is more than a target latency (~20 ms) ahead. If the
ColdFire falls behind, the repeater repeats frames (audio degrades, guest state does not). The
firmware's own cadence (Device mode, SSI0 at 96 kHz) yields ~1500 frames per guest second, so at
1.0x the queue sits near 1-3 frames.

FlexBus: in P5 the SHARC still starts from a state pack (armed_start + LP0 feed of a FlexBus log).
The machine's `RecordLink` output must equal Python's `flexbus.raw` on the same window. Feeding LP0
live into the running SHARC (sample loads without a new pack) needs an LP0 receive step in the frame
path (`scratchpad/sharc-lp0-model.md`) and is after the 1.0x gate. [O]

## 4. State import/export

Python snapshots are pickles (`emu/snapshot.py save`): `regs` (d0-7, a0-7, pc, sr only), `pages`
(zlib, 1 MiB), `all_mapped`, `mmio` (forced reads), `ctlregs`, `ff1_count`, `movec_count`, `extra`,
`components` (timers, ssi0, ...), `manifest`.

- `tools/snapconv.py` (Python, reuses `emu.snapshot._load_blob`): `.snap <-> .mstate`.
  `.mstate` = magic, JSON header (everything but pages), then `(base u32, len u32, zlib bytes)` records.
- `native/machine/src/state.rs`: `MachineState` and `read/write`. Import fills fields the snapshot
  lacks (EMAC, inactive A7, CACR/RAMBAR not in `ctlregs`) with what a Python restore leaves them
  at; this is checked on the first parity run, not assumed.
- Export writes `.mstate`; `snapconv` turns it into a `.snap`; `tools/snapeq.py py.snap native.snap`
  compares at equal clock. The native core counts `ff1`/`movec` executions so those fields match.

```rust
pub struct MachineState { pub regs: CpuRegs, pub ctlregs: BTreeMap<u32,u32>, pub pages: BTreeMap<u32, Vec<u8>>,
    pub mapped: Vec<u32>, pub mmio_forced: BTreeMap<u32,u32>, pub ff1_count: u64, pub movec_count: u64,
    pub clock: u64, pub components: Components, pub manifest: serde_json::Value }
impl<L: SharcLink> Machine<L> {
    pub fn from_state(s: &MachineState, p: &Profile, f: ImageFacts, mode: Mode, link: L) -> Result<Self, StateError>;
    pub fn export(&self) -> MachineState;
}
```

Locating a mismatch: with feature `trace`, the machine writes `DT2MMIO` v1 (the format
`native/periph/src/trace.rs` reads) through a zero-cost generic `Tracer`; `mmio-diff py.mmio
native.mmio` prints the first differing record and its clock. Then the P3 per-instruction lockstep
narrows it inside one step.

## 5. Crate layout, profile, host edges

```
native/machine/            pure, std only, builds for wasm32 (--no-default-features drops trace/state io)
  lib.rs      Machine<L>, Mode, Exit, run(), guest_time()
  board.rs    Board, Bus impl, DMA effect completion
  map.rs      Region/DevId tables
  sched.rs    deadline sources, service order, Oracle delivery, Device INTC pins
  idle.rs     IdleSpin;  hooks.rs  PC bitmap + Oracle HLEs;  link.rs  SharcLink, ZeroLink, RecordLink
  state.rs    MachineState, .mstate;  profile.rs  Profile + ImageFacts;  input.rs  input events + log
  trace.rs    (feature) DT2MMIO writer;  bin/mrun.rs  import, run N, export, trace, stats
native/live/               desktop host: QueueLink, InlineLink, pacing, CF thread, cpal (exists)
native/desktop/ (bin)      window + local webview (HTML canvas UI), wiring
native/web/ (P6)           Web Worker host, AudioWorklet + SharedArrayBuffer ring
```

```rust
pub struct Machine<L: SharcLink> { pub cpu: coldfire::Cpu, pub board: Board, sched: Sched, idle: IdleSpin,
                                   hooks: PcBitmap, mode: Mode, clock: u64, link: L }
pub enum Exit { Budget, Halted(coldfire::Stop), UnhandledVector { vector: u8, pc: u32 }, PcZero }
impl<L: SharcLink> Machine<L> {
    pub fn run(&mut self, instrs: u64) -> (u64, Exit);
    pub fn clock(&self) -> u64;
    pub fn guest_time_ns(&self) -> u64;
    pub fn input(&mut self, ev: InputEvent);        // applied at the next boundary, logged with clock
    pub fn input_log(&self) -> &[(u64, InputEvent)];
    pub fn display(&self) -> Option<&Framebuffer>;  // changed since last call
    pub fn link_mut(&mut self) -> &mut L;
}
```

- **`Profile` (product, no firmware data):** DDR/flash/SRAM sizes, peripheral set and slot map,
  PIT/DTIM channel sets and the intro rule, `ips`, SSI0 request rate (96 kHz), DSPI2 channels 28/29,
  forced MMIO defaults, panel/display geometry and key map. `Profile::DT2`, `Profile::DN2`; differences
  recorded in findings as they are found.
- **`ImageFacts` (per image, resolved on the user's machine):** image sha256, idle sites, UART8 wait
  PC, `flash_read` PC, intro handover PC, Oracle-only frame gate/counter/vector. First produced by a
  Python exporter into `out/` (JSON); later by `native/loader` from signatures.
- **Host edges:** desktop has three threads beyond the UI: ColdFire (runs `Machine::run` in ~1 ms
  guest slices, parks on the pacing condvar), producer (SHARC, exists), cpal callback (exists). Input
  crosses by an SPSC queue into `Machine::input`; the display crosses by a double buffer. The browser
  replaces only these: one Worker runs `run()` slices, the SHARC renders in the same Worker or a
  second one through a SharedArrayBuffer queue, AudioWorklet reads the ring.
- Determinism: the same snapshot plus the same input log gives the same export hash, on desktop and
  wasm. This is a test, not a hope.

## 6. Verification and order of work

Each step ends on an oracle check and a commit.

| # | step | check | kind |
|---|---|---|---|
| 1 | `.mstate` + `snapconv`; `Board` map; import then export with no execution | snapeq identical (round trip), dt2 and dn2 snaps | mechanical |
| 2 | `Cpu::run`, `take_interrupt`, PC bitmap | existing Unicorn lockstep plus unit tests for frame/SR/IPL | mechanical |
| 3 | Oracle loop: Timers, IdleSpin, UART8, forced MMIO; dt2-ready window 1M, 10M, 100M | snapeq vs `emu.longrun` exact pits mode at equal clock; mmio-diff on mismatch; resolves or refutes the P4a vec-208 "handler==0" hypothesis | design (loop), then mechanical fixes |
| 4 | DSPI2 + DSP FIFO + FrameForcer, `RecordLink`; dt2-ready-trig1 window | TX frames byte-equal to the capture; FlexBus log equal; snapeq | mechanical |
| 5 | SSI0 in Oracle mode (96 kHz), no forcer | Python with `ssi0_request_hz` on: snapeq, same 170->191 sequence and frame list | mechanical |
| 6 | DN2 windows (boot400M) | snapeq | mechanical |
| 7 | speed gate: `mrun` on the ready+SSI0 window | guest seconds per wall second >= 1.0 (target 62M useful instr/s) | measurement |
| 8 | Device mode: live INTC, STOP, no forcer | 1500 frames per guest second; trig script gives the same frame contents as Oracle; 10 guest min, no unhandled vector | design-heavy |
| 9 | desktop host: QueueLink, pacing, input log | same input log twice -> same export hash; 60 s at 1.0x, 0 underruns, both products | design, then measurement |
| 10 | webview canvas UI; cold boot DT2 to ready (needs eSDHC/GPIO/UART/panel/display lanes) | ready screen; UI and sequencer at 1.0x | mixed |

Design-heavy (Opus/deep): the scheduler and service short-circuit (1.1), Oracle vs Device delivery,
the link threading and pacing (3, step 9), Device-mode INTC (step 8). Oracle-driven mechanical work
(Sonnet/GPT, one owner per file): `.mstate`/`snapconv`, map tables from the RM, trace writer and
`mmio-diff`, profile tables, `mrun` CLI, and every mismatch fix that snapeq or mmio-diff names.

## Risks and open items

- Oracle frame SR: Python stacks `reg_read(SR)` (harness.py 410-415); unpatched Unicorn loses the
  CCR on that read (memory: "Unicorn m68k SR read clobbers flags"). If step 3 shows a CCR-only
  difference in a stacked frame, confirm which Unicorn patch covers it before changing either side.
- `ImageFacts` addresses are firmware-derived: they stay under `out/`, never in the repo.
- Python's IdleSpin vector-32 every N passes may be an emulator device, not firmware behaviour
  (longrun.py 1086-1092 calls it "the faithful mechanism"). Device mode keeps it until a run without
  it is shown to schedule correctly.
- Snapshot lacks EMAC and some control registers: import defaults must equal Python restore's.
- 96 kHz boundaries make `service` cost matter; measure it in step 7 before optimising anything else.
