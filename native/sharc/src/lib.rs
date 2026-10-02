//! Native SHARC+ core, generated from `tools/sharc_core`.
//!
//! The instruction semantics are not written here. `tools/sharc_transpile.py`
//! translates `tools/sharc_core` into Rust (`core_i`/`core_g`), and
//! `tools/sharc_rsgen.py` turns the firmware's basic blocks into functions
//! that call it with each instruction's decoded fields as constants. Both
//! write under `out/` (the block part embeds firmware); build with
//! `SHARC_GEN_DIR=<that directory>` to link them in. Without it this crate
//! is the runtime alone and every step traps.
//!
//! This crate holds the runtime the generated code runs on ([`rt`],
//! [`mem`]), the dispatcher, the harness's canonical state format
//! (tools/sharc_diff.py) and its C ABI:
//!
//! ```c
//! void*   sharc_native_create(const uint8_t* image, size_t image_len);
//! void    sharc_native_destroy(void* handle);
//! int32_t sharc_native_import_state(void* handle, const uint8_t* blob, size_t blob_len);
//! int32_t sharc_native_export_state(void* handle, uint8_t* out, size_t out_cap);
//! int32_t sharc_native_step(void* handle, uint32_t n);
//! int32_t sharc_native_halt_reason(void* handle, char* out, size_t out_cap);
//! ```
//!
//! A step that reaches something the native core does not model (a fork,
//! a stop, an unmodelled MMR, an instruction with no native code) undoes
//! that instruction and halts with a reason starting `native-trap:`; the
//! caller then lets the Python core execute it (tools/sharc_transpile_run.py).

pub mod addressing;
pub mod canon;
pub mod decode;
pub mod frames;
pub mod mem;
pub mod rt;
pub mod sha256;
pub mod vectors;

#[cfg(test)]
mod tests;

#[cfg(sharc_gen)]
#[allow(
    clippy::all,
    unused,
    non_snake_case,
    non_upper_case_globals,
    unreachable_code
)]
pub mod generated {
    pub mod syms {
        include!(concat!(env!("SHARC_GEN_DIR"), "/syms.rs"));
    }
    pub mod tables {
        include!(concat!(env!("SHARC_GEN_DIR"), "/tables.rs"));
    }
    /// The core with every function forced inline (block code).
    pub mod core_i {
        include!(concat!(env!("SHARC_GEN_DIR"), "/core_i.rs"));
    }
    /// The core with normal inlining (the one-instruction interpreter).
    pub mod core_g {
        include!(concat!(env!("SHARC_GEN_DIR"), "/core_g.rs"));
    }
    #[cfg(sharc_image)]
    pub mod image {
        include!(concat!(env!("SHARC_GEN_DIR"), "/image.rs"));
    }
}

use rt::*;

/// A block function's result.
pub const EXIT_NEXT: u32 = 0;
pub const EXIT_BUDGET: u32 = 1;
pub const EXIT_TRAP: u32 = 2;
/// Block code internal: EXIT_CHAIN + k continues in the k-th block its
/// region calls directly.
pub const EXIT_CHAIN: u32 = 16;
/// Direct block-to-block calls in a row before returning to the dispatcher.
pub const CHAIN_MAX: u32 = 32;

/// A fine-grained monotonic counter (profiling only).
#[inline(always)]
pub fn ticks() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // SAFETY: reads the virtual counter, EL0-readable on macOS.
        unsafe { core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) v) };
        v
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        std::time::UNIX_EPOCH
            .elapsed()
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

/// Counter ticks per second (ticks()).
pub fn tick_hz() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // SAFETY: reads the counter frequency register.
        unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) v) };
        v
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        1_000_000_000
    }
}

/// A chained block that was not generated: back to the dispatcher.
pub fn no_block(_s: &mut St) -> u32 {
    EXIT_NEXT
}

pub type BlockFn = fn(&mut St) -> u32;

/// pc -> block function, as a two-level table over the short-word PC.
pub struct Dispatch {
    pages: Vec<Option<Box<[Option<BlockFn>; 4096]>>>,
    pub count: usize,
}

impl Default for Dispatch {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl Dispatch {
    pub fn new(blocks: &[(u32, BlockFn)]) -> Dispatch {
        let mut pages: Vec<Option<Box<[Option<BlockFn>; 4096]>>> = Vec::new();
        pages.resize_with(1 << 12, || None);
        for &(pc, f) in blocks {
            let hi = (pc >> 12) as usize;
            if hi >= pages.len() {
                continue;
            }
            let page = pages[hi].get_or_insert_with(|| Box::new([None; 4096]));
            page[(pc & 0xFFF) as usize] = Some(f);
        }
        Dispatch {
            pages,
            count: blocks.len(),
        }
    }

    #[inline(always)]
    pub fn get(&self, pc: Int) -> Option<BlockFn> {
        if !(0..(1 << 24)).contains(&pc) {
            return None;
        }
        let pc = pc as u32;
        self.pages[(pc >> 12) as usize]
            .as_ref()
            .and_then(|p| p[(pc & 0xFFF) as usize])
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub block_entries: u64,
    pub block_instructions: u64,
    pub single_steps: u64,
    pub traps: u64,
    /// Block exits through a trap (the instruction then runs through the
    /// one-instruction interpreter).
    pub block_traps: u64,
}

pub struct Engine {
    pub s: Box<St>,
    pub halt: Option<String>,
    pub last_trap: Option<Trap>,
    pub use_blocks: bool,
    /// Diagnostic clock only: one EMUCLK tick per completed instruction.
    /// This is explicitly not a cycle-accurate DSP clock.
    pub instruction_clock: bool,
    /// Starting tick for a bounded diagnostic continuation. The host must
    /// supply it explicitly; canonical state does not carry execution counts.
    pub instruction_clock_base: u64,
    /// Diagnostic stop before an unmasked software-interrupt candidate.
    /// This observes register state; it does not emulate interrupt entry.
    pub stop_software_interrupt: bool,
    /// Opt-in instruction-boundary breakpoint; no guest state is changed.
    pub stop_pc: Option<u32>,
    /// Opt-in functional software IRQ delivery through the L1 ISA IVT.
    pub software_interrupts: bool,
    pub export_ranges: bool,
    pub dispatch: Dispatch,
    pub stats: Stats,
    /// Coverage (tools/sharc_rsgen.py --coverage): instructions run through
    /// the one-instruction interpreter, by (pc, MODE1 bits, MODE1 known).
    pub cov: Option<std::collections::HashMap<(u32, u32, bool), u64>>,
    /// Stop after one block call or one interpreted instruction (the
    /// divergence search in sharc-frames).
    pub one_dispatch: bool,
    /// Block exits into the interpreter, by (block pc, kind, pc after):
    /// kind 0 a bail at entry, 1 a trap, 2 a budget/pending exit later on.
    pub exits: Option<std::collections::HashMap<(u32, u8, u32), u64>>,
    /// Where runs of interpreted instructions start (with `cov`): the PCs
    /// block code is entered at but has no block for.
    pub entries: Option<std::collections::HashMap<u32, u64>>,
    /// Per-block timing (sharc-frames --block-profile): entry pc ->
    /// (counter ticks, instructions, entries), ticks of the generic
    /// counter (cntvct_el0 on aarch64).
    pub prof: Option<std::collections::HashMap<u32, (u64, u64, u64)>>,
    /// Block-to-block transitions (entry pc, next pc) with their counts.
    pub trans: Option<std::collections::HashMap<(u32, u32), u64>>,
    /// The PC the last interpreted instruction left (to tell a run's start).
    last_interp_next: Int,
}

/// Name of a trap: the generated site table or the runtime's own codes.
pub fn trap_name(t: Trap) -> String {
    if t.0 >= TRAP_RT_BASE {
        return rt_trap_name(t).to_string();
    }
    #[cfg(sharc_gen)]
    {
        if let Some(site) = generated::tables::TRAP_SITES.get(t.0 as usize) {
            return site.to_string();
        }
    }
    format!("trap {}", t.0)
}

#[cfg(sharc_gen)]
pub fn sym_name(s: Sym) -> &'static str {
    generated::syms::SYM_NAMES
        .get(s as usize)
        .copied()
        .unwrap_or("?")
}

#[cfg(not(sharc_gen))]
pub fn sym_name(s: Sym) -> &'static str {
    RT_SYMS.get(s as usize).copied().unwrap_or("?")
}

/// Intern NAME into the generated string table (None if it is unknown).
pub fn sym_of(name: &str) -> Option<Sym> {
    #[cfg(sharc_gen)]
    {
        generated::syms::SYM_NAMES
            .iter()
            .position(|&n| n == name)
            .map(|i| i as Sym)
    }
    #[cfg(not(sharc_gen))]
    {
        RT_SYMS.iter().position(|&n| n == name).map(|i| i as Sym)
    }
}

/// Execute one decoded instruction through the generated core, undoing it
/// if it traps.
#[inline(never)]
pub fn exec_insn(s: &mut St, insn: Insn) -> R<()> {
    #[cfg(sharc_gen)]
    {
        if s.cfg.core_timer {
            s.timer_written = false;
        }
        s.begin();
        match generated::core_g::forms::_execute(s, insn) {
            Ok(()) => s.commit(),
            Err(t) => {
                s.rollback();
                return Err(t);
            }
        }
        if s.cfg.core_timer {
            // Peripheral time advances after a completed instruction. A
            // failed timer event rolls back itself, retaining that instruction.
            s.begin();
            match generated::core_g::sequencer::_core_timer_tick(s) {
                Ok(_) => s.commit_host(),
                Err(t) => {
                    s.rollback();
                    return Err(t);
                }
            }
        }
        Ok(())
    }
    #[cfg(not(sharc_gen))]
    {
        let _ = (s, insn);
        Err(TRAP_NO_INSN)
    }
}

fn image_blocks() -> &'static [(u32, BlockFn)] {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        generated::image::BLOCKS
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        &[]
    }
}

fn image_loop_ends() -> &'static [i64] {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        generated::image::LOOP_ENDS
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        &[]
    }
}

fn image_insn_at() -> fn(Int) -> Option<Insn> {
    #[cfg(all(sharc_gen, sharc_image))]
    {
        // Build host-only decode metadata while constructing the engine.
        // The first interpreter fallback must not parse the entire image
        // and allocate its instruction table on the audio render path.
        canon::insn_table(generated::image::INSN_BLOB);
        generated::image::insn_at
    }
    #[cfg(not(all(sharc_gen, sharc_image)))]
    {
        fn none(_pc: Int) -> Option<Insn> {
            None
        }
        none
    }
}

impl Engine {
    pub fn new(mem: mem::Mem) -> Engine {
        let mut s = St::new(mem);
        s.insn_at = image_insn_at();
        s.loop_ends = image_loop_ends();
        Engine {
            s,
            halt: None,
            last_trap: None,
            use_blocks: true,
            instruction_clock: false,
            instruction_clock_base: 0,
            stop_software_interrupt: false,
            stop_pc: None,
            software_interrupts: false,
            export_ranges: false,
            dispatch: Dispatch::new(image_blocks()),
            stats: Stats::default(),
            cov: None,
            one_dispatch: false,
            exits: None,
            entries: None,
            trans: None,
            prof: None,
            last_interp_next: -1,
        }
    }

    /// An engine over an image blob (tools/sharc_transpile_run.py
    /// `pack_image`), memory at the loader image.
    pub fn from_image(bytes: &[u8]) -> Result<Engine, i32> {
        let img = canon::parse_image(bytes)?;
        let mut e = Engine::new(img.mem);
        e.s.named_mmrs = img.named;
        e.s.named_ranges = img.ranges;
        e.s.core_mmr_reset = img.core_reset;
        e.s.set_mmr_windows();
        e.s.mem.reset();
        Ok(e)
    }

    /// Select runtime instruction decoding over the engine's loaded memory.
    /// This disables firmware-specific AOT blocks and metadata for this
    /// engine only. `read_sw` maps a short-word PC to its two little-endian
    /// loaded bytes and must return None for an unmapped word.
    pub fn enable_runtime_decode(&mut self, read_sw: fn(&mem::Mem, u32) -> Option<u16>) {
        self.s.runtime_decode = true;
        self.s.read_sw = read_sw;
        self.s.decode_cache.clear();
        self.s.insn_at = |_| None;
        self.s.loop_ends = &[];
        self.dispatch = Dispatch::default();
    }

    /// Drop runtime-decoded metadata after the host replaces executable
    /// loader bytes (for example, after loader INIT blocks install main).
    pub fn invalidate_runtime_decode(&mut self) {
        self.s.decode_cache.clear();
    }

    /// Import a canonical state blob (sharc_native_import_state).
    pub fn import(&mut self, blob: &[u8]) -> Result<(), i32> {
        canon::import_state(&mut self.s, blob)?;
        self.invalidate_runtime_decode();
        self.halt = None;
        self.last_trap = None;
        Ok(())
    }

    /// memory._dm_write(ADDRESS + k*WIDTH, WIDTH, value) per WIDTH-byte
    /// chunk of DATA (a host poke). Returns how many took effect.
    pub fn poke(&mut self, address: u64, data: &[u8], width: u32) -> i32 {
        let w = width.max(1) as usize;
        let mut ok = 0;
        for (k, chunk) in data.chunks(w).enumerate() {
            let mut v: u32 = 0;
            for (i, &b) in chunk.iter().enumerate() {
                v |= (b as u32) << (8 * i);
            }
            let addr = VI::I(address as Int + (k * w) as Int);
            self.s.begin();
            match rt::bnd::_dm_write(&mut self.s, addr, chunk.len() as Int, V::c(v as Int), false) {
                Ok(true) => {
                    self.s.commit_host();
                    ok += 1
                }
                Ok(false) => self.s.commit_host(),
                Err(_) => self.s.rollback(),
            }
        }
        ok
    }

    /// memory._dm_read(ADDRESS, WIDTH): Ok(None) when unknown, Err on an
    /// unmodelled MMR.
    pub fn peek(&self, address: u64, width: u32) -> Result<Option<u32>, Trap> {
        rt::bnd::_dm_read(&self.s, VI::I(address as Int), width as Int, false, false)
            .map(|v| v.map(|v| v.b))
    }

    /// sharc_run.fresh_call_state (see sharc_native_fresh_call).
    pub fn fresh_call(&mut self, pc: u32, return_address: Option<Int>) {
        self.s.pc_sw = pc as Int;
        self.s.loops.clear();
        self.s.call_stack.clear();
        self.s.pc_stack.clear();
        self.s.pc_stack_pending = -1;
        self.s.pc_stack_requested = -1;
        if let Some(r) = return_address {
            let _ = self.s.call_stack.push_raw(r);
            if self.s.cfg.stack_model {
                let _ = self.s.pc_stack.push_raw(0x0100_0000 | (r & 0x00ff_ffff));
            }
        }
        self.s.status_stack.clear();
        self.s.pending = None;
        #[cfg(sharc_gen)]
        if self.s.cfg.stack_model {
            let _ = generated::core_g::state::_sync_pc_stack(&mut self.s);
            let _ = generated::core_g::state::_sync_status_stack(&mut self.s);
            self.s.commit_host();
        }
        self.halt = None;
        self.last_trap = None;
    }

    /// A host register poke (sharc_native_set_reg).
    pub fn set_reg(&mut self, code: usize, v: V) {
        self.s.r[code] = v;
    }

    /// sharc_native_set_option.
    pub fn set_option(&mut self, key: u32, value: i64) -> i32 {
        match key {
            1 => self.use_blocks = value != 0,
            2 => self.export_ranges = value != 0,
            4 => {
                if value == 0 {
                    return -1;
                }
                self.enable_runtime_decode(mem::Mem::read_sw);
            }
            5 => self.instruction_clock = value != 0,
            7 => self.stop_software_interrupt = value != 0,
            9 => self.software_interrupts = value != 0,
            8 => {
                if value == -1 {
                    self.stop_pc = None;
                } else if (0..=0x00ff_ffff).contains(&value) {
                    self.stop_pc = Some(value as u32);
                } else {
                    return -1;
                }
            }
            6 => {
                if value < 0 {
                    return -1;
                }
                self.instruction_clock_base = value as u64;
            }
            3 => {
                for i in 0..7 {
                    self.s.special_present[i] = value & (1 << i) != 0;
                }
            }
            _ => return canon::set_option(&mut self.s, key, value),
        }
        0
    }

    fn software_interrupt_candidate(&self) -> Option<u32> {
        if self.s.pending.is_some() || !self.s.loops.items().is_empty() {
            return None;
        }
        let [mode, latch, mask, priority] = [114, 122, 123, 124].map(|code| self.s.r[code]);
        if ![mode, latch, mask, priority]
            .iter()
            .all(|value| value.is_c())
            || mode.b & (1 << 12) == 0
        {
            return None;
        }
        let mut candidates = latch.b & mask.b & 0xf000_0000;
        if priority.b != 0 {
            if mode.b & (1 << 11) == 0 {
                return None;
            }
            candidates &= ((1u64 << priority.b.trailing_zeros()) - 1) as u32;
        }
        (candidates != 0).then(|| candidates.trailing_zeros())
    }

    fn trapped(&mut self, t: Trap) {
        self.stats.traps += 1;
        self.last_trap = Some(t);
        self.halt = Some(format!(
            "native-trap: {} at {:#x}",
            trap_name(t),
            self.s.pc_sw
        ));
    }

    /// Run up to N instructions. Returns how many completed; fewer means a
    /// trap (see `halt`).
    pub fn step(&mut self, n: u32) -> u32 {
        if self.halt.is_some() {
            return 0;
        }
        let start = self.s.icount;
        let limit = start + n as u64;
        while self.s.icount < limit {
            if self.stop_pc.is_some_and(|pc| self.s.pc_sw == pc as Int) {
                self.halt = Some("diagnostic: PC breakpoint".into());
                break;
            }
            if self.stop_software_interrupt && self.software_interrupt_candidate().is_some() {
                self.halt =
                    Some("diagnostic: unmasked software interrupt; entry not modeled".into());
                break;
            }
            if self.software_interrupts || self.s.cfg.core_timer {
                #[cfg(sharc_gen)]
                {
                    let allowed = (if self.software_interrupts {
                        0xf000_0000
                    } else {
                        0
                    }) | (if self.s.cfg.core_timer {
                        0x0040_0800
                    } else {
                        0
                    });
                    let candidate =
                        generated::core_g::sequencer::_interrupt_candidate(&mut self.s, allowed);
                    let result = candidate.and_then(|mask| {
                        if mask == 0 {
                            return Ok(());
                        }
                        self.s.begin();
                        let result =
                            generated::core_g::sequencer::_enter_interrupt(&mut self.s, mask);
                        if result.is_ok() {
                            self.s.bank_complete();
                            self.s.bank_complete();
                            self.s.commit_host();
                        } else {
                            self.s.rollback();
                        }
                        result
                    });
                    if let Err(trap) = result {
                        self.trapped(trap);
                        break;
                    }
                }
                #[cfg(not(sharc_gen))]
                {
                    self.trapped(TRAP_NO_INSN);
                    break;
                }
            }
            if self.instruction_clock {
                let tick = self.instruction_clock_base.wrapping_add(self.s.icount);
                self.s.r[105] = V::c((tick as u32) as Int);
                self.s.r[106] = V::c((tick >> 32) as Int);
            }
            if !self.instruction_clock
                && !self.software_interrupts
                && !self.stop_software_interrupt
                && self.stop_pc.is_none()
                && self.use_blocks
                && self.s.cfg.block_ok
                && self.s.loops_ok
                && let Some(f) = self.dispatch.get(self.s.pc_sw)
            {
                self.s.limit = limit;
                self.s.chain = 0;
                let before = self.s.icount;
                let entry = self.s.pc_sw as u32;
                self.last_interp_next = -1;
                let t0 = if self.prof.is_some() { ticks() } else { 0 };
                let code = f(&mut self.s);
                if let Some(p) = &mut self.prof {
                    let t = ticks() - t0;
                    let e = p.entry(entry).or_default();
                    e.0 += t;
                    e.1 += self.s.icount - before;
                    e.2 += 1;
                }
                if let Some(t) = &mut self.trans {
                    *t.entry((entry, self.s.pc_sw as u32)).or_default() += 1;
                }
                if code != EXIT_NEXT
                    && let Some(x) = &mut self.exits
                {
                    let (kind, at) = if code == EXIT_TRAP {
                        (1, self.s.trap.map(|t| t.0).unwrap_or(0))
                    } else if self.s.icount == before {
                        (0, self.s.pc_sw as u32)
                    } else {
                        (2, self.s.pc_sw as u32)
                    };
                    *x.entry((entry, kind, at)).or_default() += 1;
                }
                self.stats.block_entries += 1;
                self.stats.block_instructions += self.s.icount - before;
                if self.one_dispatch && self.s.icount > before {
                    break;
                }
                match code {
                    EXIT_NEXT => continue,
                    EXIT_TRAP => {
                        // Undone: the one-instruction interpreter runs it
                        // (and traps for good if the core does).
                        self.stats.block_traps += 1;
                        self.s.trap = None;
                        if self.s.icount >= limit {
                            break;
                        }
                    }
                    _ => {
                        if self.s.icount >= limit {
                            break;
                        }
                    }
                }
            }
            let insn = if self.s.runtime_decode {
                let pc = self.s.pc_sw;
                match rt::bnd::decode_at(&mut self.s, (), None, pc) {
                    Ok(insn) => insn,
                    Err(t) => {
                        self.trapped(t);
                        break;
                    }
                }
            } else if let Some(insn) = (self.s.insn_at)(self.s.pc_sw) {
                insn
            } else {
                self.trapped(TRAP_NO_INSN);
                break;
            };
            self.stats.single_steps += 1;
            if let Some(cov) = &mut self.cov {
                let m = self.s.r[MODE1];
                *cov.entry((self.s.pc_sw as u32, m.b, m.is_c())).or_default() += 1;
                if let Some(en) = &mut self.entries
                    && self.s.pc_sw != self.last_interp_next
                {
                    *en.entry(self.s.pc_sw as u32).or_default() += 1;
                }
            }
            if let Err(t) = exec_insn(&mut self.s, insn) {
                self.trapped(t);
                break;
            }
            self.last_interp_next = self.s.pc_sw;
            if self.one_dispatch {
                break;
            }
        }
        (self.s.icount - start) as u32
    }
}

// ---------------------------------------------------------------------------
// C ABI (tools/sharc_diff.py NativeEngine, tools/sharc_transpile_run.py)
// ---------------------------------------------------------------------------

/// Allocate a host-transfer buffer, including in WebAssembly where JavaScript
/// cannot supply an allocation owned by Rust. Release with the same length.
#[unsafe(no_mangle)]
pub extern "C" fn sharc_native_alloc(len: usize) -> *mut u8 {
    let bytes = vec![0u8; len].into_boxed_slice();
    Box::into_raw(bytes) as *mut u8
}

/// # Safety
/// PTR and LEN must be an outstanding allocation from sharc_native_alloc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        // SAFETY: caller supplies the allocation and its original length.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
    }
}

/// # Safety
/// IMAGE must point to IMAGE_LEN readable bytes (a tools/sharc_transpile_run.py
/// `pack_image` blob).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_create(image: *const u8, image_len: usize) -> *mut Engine {
    let bytes = if image.is_null() {
        &[][..]
    } else {
        // SAFETY: the caller guarantees IMAGE_LEN readable bytes.
        unsafe { std::slice::from_raw_parts(image, image_len) }
    };
    match Engine::from_image(bytes) {
        Ok(e) => Box::into_raw(Box::new(e)),
        Err(_) => std::ptr::null_mut(),
    }
}

/// # Safety
/// HANDLE must come from sharc_native_create and not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_destroy(handle: *mut Engine) {
    if !handle.is_null() {
        // SAFETY: created by Box::into_raw in sharc_native_create.
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// # Safety
/// HANDLE from sharc_native_create; BLOB points to BLOB_LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_import_state(
    handle: *mut Engine,
    blob: *const u8,
    blob_len: usize,
) -> i32 {
    // SAFETY: caller contract.
    let (e, bytes) = unsafe { (&mut *handle, std::slice::from_raw_parts(blob, blob_len)) };
    match e.import(bytes) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

/// # Safety
/// HANDLE from sharc_native_create; OUT points to OUT_CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_export_state(
    handle: *mut Engine,
    out: *mut u8,
    out_cap: usize,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let blob = canon::export_state(&e.s, e.export_ranges);
    if blob.len() > out_cap {
        return -(blob.len() as i32);
    }
    // SAFETY: OUT has OUT_CAP >= blob.len() writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(blob.as_ptr(), out, blob.len()) };
    blob.len() as i32
}

/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_step(handle: *mut Engine, n: u32) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.step(n) as i32
}

/// # Safety
/// HANDLE from sharc_native_create; OUT points to OUT_CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_halt_reason(
    handle: *mut Engine,
    out: *mut u8,
    out_cap: usize,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let Some(reason) = &e.halt else {
        return 0;
    };
    let n = reason.len().min(out_cap);
    // SAFETY: OUT has OUT_CAP >= n writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(reason.as_ptr(), out, n) };
    n as i32
}

/// Options beyond the harness contract. KEY: 1 use block code (default
/// 1), 2 export every overlay byte as memory ranges (default 0), 3 which
/// special-register slots are present as dict keys (bit per
/// SPECIAL_SLOTS entry; the canonical format cannot say, and
/// `"BFF_HI" in special` reads it), 4 enables runtime decoding,
/// 5 enables the diagnostic instruction clock, 10+ run configuration
/// (canon::set_option).
///
/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_set_option(handle: *mut Engine, key: u32, value: i64) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.set_option(key, value)
}

/// state.provisional_interpretations[NAME] = MODE (both UTF-8).
///
/// # Safety
/// HANDLE from sharc_native_create; the pointers cover their lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_set_provisional(
    handle: *mut Engine,
    name: *const u8,
    name_len: usize,
    mode: *const u8,
    mode_len: usize,
) -> i32 {
    // SAFETY: caller contract.
    let (e, name, mode) = unsafe {
        (
            &mut *handle,
            std::slice::from_raw_parts(name, name_len),
            std::slice::from_raw_parts(mode, mode_len),
        )
    };
    let (Ok(name), Ok(mode)) = (std::str::from_utf8(name), std::str::from_utf8(mode)) else {
        return -1;
    };
    let (Some(n), Some(m)) = (sym_of(name), sym_of(mode)) else {
        return -2;
    };
    e.s.cfg.provisional_interp.retain(|(k, _)| *k != n);
    e.s.cfg.provisional_interp.push((n, m));
    e.s.cfg.refresh();
    0
}

/// Execute one instruction described by BLOB (canon::parse_insn: type
/// name, length, kind, fields) through the generated core, without the
/// image's instruction table: the compute corpus's fabricated cases.
/// Returns 1 when it completed, 0 when it trapped (see halt_reason), <0
/// for a malformed blob.
///
/// # Safety
/// HANDLE from sharc_native_create; BLOB points to BLOB_LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_exec_insn(
    handle: *mut Engine,
    blob: *const u8,
    blob_len: usize,
) -> i32 {
    // SAFETY: caller contract.
    let (e, bytes) = unsafe { (&mut *handle, std::slice::from_raw_parts(blob, blob_len)) };
    let insn = match canon::parse_insn(bytes) {
        Ok(i) => i,
        Err(code) => return code,
    };
    match exec_insn(&mut e.s, insn) {
        Ok(()) => 1,
        Err(t) => {
            e.trapped(t);
            0
        }
    }
}

/// Counters: [instructions, block entries, block instructions, single
/// steps, traps, blocks in the image, special-slot presence bits].
/// Returns how many were written.
///
/// # Safety
/// HANDLE from sharc_native_create; OUT points to OUT_CAP u64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_stats(
    handle: *mut Engine,
    out: *mut u64,
    out_cap: usize,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let v = [
        e.s.icount,
        e.stats.block_entries,
        e.stats.block_instructions,
        e.stats.single_steps,
        e.stats.traps,
        e.dispatch.count as u64,
        (0..7).map(|i| (e.s.special_present[i] as u64) << i).sum(),
    ];
    let n = v.len().min(out_cap);
    // SAFETY: OUT has OUT_CAP >= n u64s.
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n as i32
}

/// Set UREG CODE to (KIND 0 Unknown / 1 Const / 2 PartialConst, VALUE,
/// MASK), as a host poke between runs.
///
/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_set_reg(
    handle: *mut Engine,
    code: u32,
    kind: u32,
    value: u32,
    mask: u32,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    if code as usize >= NUREG {
        return -1;
    }
    let v = match kind {
        1 => V::c(value as Int),
        2 => V::partial(mask as Int, value as Int),
        _ => V::UNK,
    };
    e.set_reg(code as usize, v);
    0
}

/// UREG CODE as (value, mask) packed into a u64 (mask in the high half).
///
/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_get_reg(handle: *mut Engine, code: u32) -> u64 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    let v = e.s.r[code as usize % NUREG];
    ((v.m as u64) << 32) | v.b as u64
}

/// The architectural software PC. Returns -1 for a null HANDLE.
///
/// Unlike UREG `PC`, which is a separately modelled register slot, this is
/// `State.pc_sw`, the address the single-step dispatcher will execute next.
///
/// # Safety
/// HANDLE from sharc_native_create, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_get_pc(handle: *mut Engine) -> i64 {
    if handle.is_null() {
        return -1;
    }
    // SAFETY: checked non-null HANDLE is from sharc_native_create.
    unsafe { (&*handle).s.pc_sw as i64 }
}

/// memory._dm_write(state, ADDRESS + i, 1, byte) for each byte (a host
/// poke, as sharc_harness._poke does). Returns how many took effect.
///
/// # Safety
/// HANDLE from sharc_native_create; DATA points to LEN bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_poke(
    handle: *mut Engine,
    address: u64,
    data: *const u8,
    len: usize,
    width: u32,
) -> i32 {
    // SAFETY: caller contract.
    let (e, bytes) = unsafe { (&mut *handle, std::slice::from_raw_parts(data, len)) };
    e.poke(address, bytes, width)
}

/// memory._dm_read(state, ADDRESS, WIDTH): the value in the low 32 bits,
/// bit 32 set when known, -1 on an unmodelled MMR.
///
/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_peek(handle: *mut Engine, address: u64, width: u32) -> i64 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    match e.peek(address, width) {
        Ok(Some(v)) => (1i64 << 32) | v as i64,
        Ok(None) => 0,
        Err(_) => -1,
    }
}

/// sharc_run.fresh_call_state: a new call at PC with empty loop, PC and
/// status stacks, no delayed transfer and no stop (RETURN_ADDRESS >= 0
/// becomes the PC stack's only entry).
///
/// # Safety
/// HANDLE from sharc_native_create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_fresh_call(
    handle: *mut Engine,
    pc: u32,
    return_address: i64,
) -> i32 {
    // SAFETY: caller contract.
    let e = unsafe { &mut *handle };
    e.fresh_call(pc, (return_address >= 0).then_some(return_address as Int));
    0
}

/// A JSON description of the build: generated core and image hashes.
///
/// # Safety
/// OUT points to OUT_CAP writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sharc_native_info(out: *mut u8, out_cap: usize) -> i32 {
    let info = build_info();
    if info.len() > out_cap {
        return -(info.len() as i32);
    }
    // SAFETY: OUT has OUT_CAP >= len writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(info.as_ptr(), out, info.len()) };
    info.len() as i32
}

/// The build's description (sharc_native_info): the SHA-256 of the
/// tools/sharc_core sources and the generator version the generated code
/// came from (0: generated before the version existed), the image's
/// SHA-256 and the block count. Loaders compare the first two with the
/// current sources and refuse a stale library
/// (tools/sharc_transpile_run.check_build_info, native/live).
#[allow(unused_assignments)]
pub fn build_info() -> String {
    #[allow(unused_mut)]
    let mut core = "none";
    #[allow(unused_mut)]
    let mut generator: u32 = 0;
    #[allow(unused_mut)]
    let mut image = "none";
    #[cfg(sharc_gen)]
    {
        core = generated::tables::CORE_SHA256;
    }
    #[cfg(all(sharc_gen, sharc_gen_version))]
    {
        generator = generated::tables::GENERATOR_VERSION;
    }
    #[cfg(all(sharc_gen, sharc_image))]
    {
        image = generated::image::IMAGE_SHA256;
    }
    format!(
        "{{\"core_sha256\": \"{core}\", \"generator_version\": {generator}, \"image_sha256\": \"{image}\", \"blocks\": {}}}",
        image_blocks().len()
    )
}
