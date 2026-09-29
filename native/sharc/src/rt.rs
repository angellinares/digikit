//! The hand-written runtime under the transpiled SHARC+ core.
//!
//! `tools/sharc_transpile.py` translates `tools/sharc_core` into Rust that
//! calls this module for the boundary `tools/sharc_core/SUBSET.md` declares:
//! the value lattice (`values.py`), the register and accumulator access in
//! `state.py`, the byte store under `memory.py` (loader image plus write
//! overlay), and the float primitives in `floats.py`. Each function in
//! [`bnd`] names the Python function it stands for and follows it line by
//! line; the concrete specialisation maps `Const` to a fully known [`V`],
//! `Unknown` to an unknown one, and anything symbolic (`Affine`) to a trap.
//!
//! Every mutation of the machine state goes through [`St`]'s journal so a
//! trap can undo the instruction it happened in: the Python core then
//! executes that instruction itself.

use crate::mem::Mem;

pub type Int = i128;
pub type Sym = u16;
pub type FnId = u16;
pub type TblId = u16;

/// Interned strings the runtime itself needs. `tools/sharc_transpile.py`
/// interns these first, in this order.
pub const S_EMPTY: Sym = 0;
pub const SYM_DYN: Sym = 1;
pub const RT_SYMS: [&str; 16] = [
    "",
    "<dyn>",
    "MRF",
    "MRB",
    "MSF",
    "MSB",
    "BFFWRP",
    "BFF_HI",
    "BFF_LO",
    "unknown",
    "confident",
    "uncertain",
    "nop",
    "DM",
    "PM",
    "21p_undoc16",
];
pub const S_MRF: Sym = 2;
pub const S_MRB: Sym = 3;
pub const S_MSF: Sym = 4;
pub const S_MSB: Sym = 5;
pub const S_BFFWRP: Sym = 6;
pub const S_BFF_HI: Sym = 7;
pub const S_BFF_LO: Sym = 8;
pub const S_UNKNOWN: Sym = 9;
pub const S_CONFIDENT: Sym = 10;
pub const S_UNCERTAIN: Sym = 11;
pub const S_NOP: Sym = 12;
pub const S_DM: Sym = 13;
pub const S_PM: Sym = 14;
pub const S_21P_UNDOC16: Sym = 15;
/// The special-register slots, in the harness's SPECIAL_SLOTS order.
pub const SPECIAL_SLOT_SYMS: [Sym; 7] = [2, 3, 4, 5, 6, 7, 8];

/// FnIds of values.py's named integer operations (the `operation`
/// argument of `_bitwise`).
pub const FN_OP_AND: FnId = 0;
pub const FN_OP_OR: FnId = 1;
pub const FN_OP_XOR: FnId = 2;
pub const FN_OP_ANDNOT: FnId = 3;

/// Why native execution stopped before finishing an instruction. The
/// number indexes the generated trap-site table (tables.rs TRAP_SITES) or
/// is one of the runtime's own codes below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trap(pub u32);
pub type R<T> = Result<T, Trap>;

pub const TRAP_RT_BASE: u32 = 0x8000_0000;
pub const TRAP_UNMODELED_MMR: Trap = Trap(TRAP_RT_BASE + 1);
pub const TRAP_SYMBOLIC: Trap = Trap(TRAP_RT_BASE + 2);
pub const TRAP_STACK_FULL: Trap = Trap(TRAP_RT_BASE + 3);
pub const TRAP_INDEX: Trap = Trap(TRAP_RT_BASE + 4);
pub const TRAP_ARITH: Trap = Trap(TRAP_RT_BASE + 5);
pub const TRAP_NO_INSN: Trap = Trap(TRAP_RT_BASE + 6);
pub const TRAP_KEY: Trap = Trap(TRAP_RT_BASE + 7);
pub const TRAP_JOURNAL: Trap = Trap(TRAP_RT_BASE + 8);
pub const TRAP_ADDRESS: Trap = Trap(TRAP_RT_BASE + 9);
pub const TRAP_NO_BLOCK: Trap = Trap(TRAP_RT_BASE + 10);
/// Block code only: a register would become Unknown or PartialConst in a
/// way block code does not follow (the one-instruction interpreter runs
/// the instruction instead).
pub const TRAP_BLOCK_UNKNOWN: Trap = Trap(TRAP_RT_BASE + 11);

pub fn rt_trap_name(t: Trap) -> &'static str {
    match t.0.wrapping_sub(TRAP_RT_BASE) {
        1 => "unmodeled MMR",
        2 => "symbolic value",
        3 => "native stack full",
        4 => "index out of range",
        5 => "arithmetic error",
        6 => "no decoded instruction",
        7 => "missing key",
        8 => "journal full",
        9 => "address outside 32 bits",
        10 => "no native code for this pc",
        11 => "block code: value not known",
        _ => "?",
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A 32-bit register value known bit by bit: `m` has a 1 at every known
/// bit, `b` the value there (0 elsewhere). `Const` is m = !0, `Unknown`
/// m = 0, `PartialConst` (ASTATX/ASTATY only) anything between.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V {
    pub b: u32,
    pub m: u32,
}

impl V {
    pub const UNK: V = V { b: 0, m: 0 };
    #[inline(always)]
    pub fn c(x: Int) -> V {
        V {
            b: x as u32,
            m: u32::MAX,
        }
    }
    #[inline(always)]
    pub fn partial(mask: Int, bits: Int) -> V {
        let m = mask as u32;
        V {
            b: bits as u32 & m,
            m,
        }
    }
    #[inline(always)]
    pub fn is_c(self) -> bool {
        self.m == u32::MAX
    }
    #[inline(always)]
    pub fn is_unknown(self) -> bool {
        self.m == 0
    }
    #[inline(always)]
    pub fn is_partial(self) -> bool {
        self.m != 0 && self.m != u32::MAX
    }
    #[inline(always)]
    pub fn val(self) -> Int {
        self.b as Int
    }
}

/// An 80-bit multiplier accumulator known bit by bit (state.MR).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MR {
    pub mask: Int,
    pub bits: Int,
}

pub const MR_MASK: Int = (1 << 80) - 1;

impl MR {
    #[inline(always)]
    pub fn new(mask: Int, bits: Int) -> MR {
        let mask = mask & MR_MASK;
        MR {
            mask,
            bits: bits & mask,
        }
    }
    #[inline(always)]
    pub fn known(self) -> bool {
        self.mask == MR_MASK
    }
    #[inline(always)]
    pub fn signed(self) -> Option<Int> {
        if !self.known() {
            return None;
        }
        Some(if self.bits & (1 << 79) != 0 {
            self.bits - (1 << 80)
        } else {
            self.bits
        })
    }
}

/// `Operand | MR`: a special-register slot's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spec {
    V(V),
    M(MR),
}

impl Default for Spec {
    fn default() -> Self {
        Spec::V(V::UNK)
    }
}

impl Spec {
    #[inline(always)]
    pub fn as_v(self) -> V {
        match self {
            Spec::V(v) => v,
            Spec::M(_) => V::UNK,
        }
    }
    #[inline(always)]
    pub fn as_mr(self) -> MR {
        match self {
            Spec::M(m) => m,
            Spec::V(_) => MR::default(),
        }
    }
    #[inline(always)]
    pub fn is_c(self) -> bool {
        matches!(self, Spec::V(v) if v.is_c())
    }
    #[inline(always)]
    pub fn is_unknown(self) -> bool {
        matches!(self, Spec::V(v) if v.is_unknown())
    }
    #[inline(always)]
    pub fn is_partial(self) -> bool {
        matches!(self, Spec::V(v) if v.is_partial())
    }
    #[inline(always)]
    pub fn is_mr(self) -> bool {
        matches!(self, Spec::M(_))
    }
}

/// `Value | int`: a memory address argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VI {
    V(V),
    I(Int),
}

impl Default for VI {
    fn default() -> Self {
        VI::V(V::UNK)
    }
}

impl VI {
    #[inline(always)]
    pub fn is_c(self) -> bool {
        match self {
            VI::V(v) => v.is_c(),
            VI::I(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// values.FlagUpdate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlagUpdate {
    pub define_mask: Int,
    pub define_bits: Int,
    pub forget_mask: Int,
    pub cacc: Int,
}

/// state.Pending: a delayed transfer in flight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub target: Option<Int>,
    pub call: bool,
    pub slots: Int,
    pub return_from_call: bool,
    pub return_sw: Option<Int>,
}

/// state.Loop: a hardware DO loop stack entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loop {
    // i64, not Int: 32-bit machine values (tools/sharc_transpile.py
    // NARROW_FIELDS converts at every access).
    pub start_sw: i64,
    pub end_sw: i64,
    pub remaining: i64,
    pub mode: i64,
}

/// One decoded field: KEY (e.g. "data[31:16]"), its STEM (the key up to
/// the first '[': `_field`'s prefix match), the bit range HI:LO the key
/// names (-1 when it names none) and the value.
#[derive(Clone, Copy, Debug)]
pub struct FieldEntry(pub Sym, pub Sym, pub i8, pub i8, pub Int);

/// Decoded fields of one instruction, in decode order (a Python dict's
/// insertion order).
#[derive(Debug)]
pub struct Fields {
    pub kv: &'static [FieldEntry],
}

impl Fields {
    #[inline(always)]
    pub fn get(&self, k: Sym) -> Option<Int> {
        for e in self.kv {
            if e.0 == k {
                return Some(e.4);
            }
        }
        None
    }
    #[inline(always)]
    pub fn key(&self, k: Sym) -> R<Int> {
        self.get(k).ok_or(TRAP_KEY)
    }
    #[inline(always)]
    pub fn contains(&self, k: Sym) -> bool {
        self.get(k).is_some()
    }
}

pub static FIELDS_NONE: Fields = Fields { kv: &[] };

impl Default for &'static Fields {
    fn default() -> Self {
        &FIELDS_NONE
    }
}

/// sharc_disasm.Instruction, as the core reads it.
#[derive(Debug)]
pub struct Insn {
    pub type_name: Sym,
    pub fields: &'static Fields,
    pub length_bytes: Option<Int>,
    pub kind: Sym,
    pub offset: Int,
}

pub static INSN_NONE: Insn = Insn {
    type_name: S_UNKNOWN,
    fields: &FIELDS_NONE,
    length_bytes: None,
    kind: S_UNKNOWN,
    offset: 0,
};

impl Default for &'static Insn {
    fn default() -> Self {
        &INSN_NONE
    }
}

// ---------------------------------------------------------------------------
// Small fixed-capacity sequences (Python lists and variable tuples)
// ---------------------------------------------------------------------------

pub const TUP_CAP: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tup<T: Copy + Default> {
    n: u8,
    a: [T; TUP_CAP],
}

impl<T: Copy + Default> Default for Tup<T> {
    fn default() -> Self {
        Tup {
            n: 0,
            a: [T::default(); TUP_CAP],
        }
    }
}

impl<T: Copy + Default + PartialEq> Tup<T> {
    #[inline(always)]
    pub fn new() -> Self {
        Self::default()
    }
    #[inline(always)]
    pub fn from_slice(items: &[T]) -> Self {
        let mut t = Self::default();
        let n = items.len().min(TUP_CAP);
        t.a[..n].copy_from_slice(&items[..n]);
        t.n = n as u8;
        t
    }
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.n as usize
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    #[inline(always)]
    pub fn get(&self, i: usize) -> T {
        self.a[i]
    }
    #[inline(always)]
    fn index(&self, i: Int) -> R<usize> {
        let n = self.n as Int;
        let j = if i < 0 { i + n } else { i };
        if j < 0 || j >= n {
            return Err(TRAP_INDEX);
        }
        Ok(j as usize)
    }
    #[inline(always)]
    pub fn at(&self, i: Int) -> R<T> {
        Ok(self.a[self.index(i)?])
    }
    #[inline(always)]
    pub fn set(&mut self, i: Int, v: T) -> R<()> {
        let j = self.index(i)?;
        self.a[j] = v;
        Ok(())
    }
    #[inline(always)]
    pub fn push(&mut self, v: T) -> R<()> {
        if self.n as usize >= TUP_CAP {
            return Err(TRAP_STACK_FULL);
        }
        self.a[self.n as usize] = v;
        self.n += 1;
        Ok(())
    }
    #[inline(always)]
    pub fn slice(&self, lo: Int, hi: Int) -> Tup<T> {
        let n = self.n as Int;
        let norm = |x: Int| -> usize {
            let y = if x < 0 { x + n } else { x };
            y.clamp(0, n) as usize
        };
        let (a, b) = (norm(lo), norm(hi));
        if b <= a {
            return Tup::new();
        }
        Tup::from_slice(&self.a[a..b])
    }
    #[inline(always)]
    pub fn contains(&self, v: T) -> bool {
        self.a[..self.n as usize].contains(&v)
    }
    #[inline(always)]
    pub fn map<U: Copy + Default + PartialEq>(&self, f: impl Fn(T) -> U) -> Tup<U> {
        let mut out = Tup::<U>::default();
        for i in 0..self.n as usize {
            out.a[i] = f(self.a[i]);
        }
        out.n = self.n;
        out
    }
}

#[inline(always)]
pub fn tup_concat<T: Copy + Default + PartialEq>(a: Tup<T>, b: Tup<T>) -> Tup<T> {
    let mut out = a;
    for i in 0..b.len() {
        let _ = out.push(b.get(i));
    }
    out
}

#[inline(always)]
pub fn tup_index<T: Copy>(items: &[T], i: Int) -> R<T> {
    let n = items.len() as Int;
    let j = if i < 0 { i + n } else { i };
    if j < 0 || j >= n {
        return Err(TRAP_INDEX);
    }
    Ok(items[j as usize])
}

/// A fixed-capacity stack (the loop, PC and status stacks).
#[derive(Clone, Copy, Debug)]
pub struct Stk<T: Copy + Default, const N: usize> {
    pub n: usize,
    pub a: [T; N],
}

impl<T: Copy + Default, const N: usize> Default for Stk<T, N> {
    fn default() -> Self {
        Stk {
            n: 0,
            a: [T::default(); N],
        }
    }
}

impl<T: Copy + Default + PartialEq, const N: usize> Stk<T, N> {
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.n
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    #[inline(always)]
    pub fn items(&self) -> &[T] {
        &self.a[..self.n]
    }
    #[inline(always)]
    fn index(&self, i: Int) -> R<usize> {
        let n = self.n as Int;
        let j = if i < 0 { i + n } else { i };
        if j < 0 || j >= n {
            return Err(TRAP_INDEX);
        }
        Ok(j as usize)
    }
    pub fn clear(&mut self) {
        self.n = 0;
    }
    pub fn push_raw(&mut self, v: T) -> R<()> {
        if self.n >= N {
            return Err(TRAP_STACK_FULL);
        }
        self.a[self.n] = v;
        self.n += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Machine state
// ---------------------------------------------------------------------------

pub const NUREG: usize = 128;
pub const UREG_PX: usize = 107;
pub const MODE1: usize = 114;
pub const UREG_PX1: usize = 108;
pub const UREG_PX2: usize = 109;
pub const MAX_LOOPS: usize = 16;
pub const MAX_CALLS: usize = 256;
pub const MAX_STATUS: usize = 16;

/// Run configuration (State fields that do not change during a run).
#[derive(Clone, Debug)]
pub struct Cfg {
    pub has_concrete: bool,
    pub follow_loaded_calls: bool,
    pub continue_external_calls: bool,
    pub max_call_depth: Int,
    pub assume_nw32: bool,
    pub explicit_memory_model: bool,
    pub approx_recips: bool,
    pub data_memory_tainted: bool,
    pub dossier_bytes: Int,
    /// provisional_interpretations: form name -> mode ("nop").
    pub provisional_interp: Vec<(Sym, Sym)>,
    /// provisional_forms.
    pub provisional_forms: Vec<Sym>,
    /// The configuration is the one block code was generated for
    /// (tools/sharc_transpile.py GEN_CFG_DEFAULT); block code runs only then.
    pub block_ok: bool,
    /// The configuration allows the plain-RAM fast paths (has_concrete,
    /// assume_nw32, no data_memory_tainted).
    pub fast_mem: bool,
}

impl Default for Cfg {
    /// sharc_harness._make_runner / sharc_diff.import_state defaults.
    fn default() -> Self {
        Cfg {
            has_concrete: true,
            follow_loaded_calls: true,
            continue_external_calls: false,
            max_call_depth: 64,
            assume_nw32: true,
            explicit_memory_model: true,
            approx_recips: true,
            data_memory_tainted: false,
            dossier_bytes: 0,
            provisional_interp: Vec::new(),
            provisional_forms: Vec::new(),
            block_ok: true,
            fast_mem: true,
        }
    }
}

impl Cfg {
    /// Recompute `block_ok` after a change.
    pub fn refresh(&mut self) {
        let d = Cfg::default();
        self.fast_mem = self.has_concrete && self.assume_nw32 && !self.data_memory_tainted;
        self.block_ok = self.has_concrete == d.has_concrete
            && self.follow_loaded_calls == d.follow_loaded_calls
            && self.continue_external_calls == d.continue_external_calls
            && self.max_call_depth == d.max_call_depth
            && self.assume_nw32 == d.assume_nw32
            && self.explicit_memory_model == d.explicit_memory_model
            && self.approx_recips == d.approx_recips
            && self.data_memory_tainted == d.data_memory_tainted
            && self.dossier_bytes == d.dossier_bytes
            && self.provisional_interp.is_empty()
            && self.provisional_forms.is_empty();
    }
    pub fn provisional_get(&self, name: Sym) -> Option<Sym> {
        self.provisional_interp
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| *v)
    }
    pub fn provisional_interp_contains(&self, name: Sym) -> bool {
        self.provisional_get(name).is_some()
    }
    pub fn provisional_form(&self, name: Sym) -> bool {
        self.provisional_forms.contains(&name)
    }
}

/// What a trap has to undo, besides registers and the per-instruction
/// scalars saved at `begin`.
#[derive(Clone, Copy, Debug)]
pub enum Undo {
    Special(u8, Spec, bool),
    LoopsPush,
    LoopsPop(Loop),
    LoopsSet(usize, Loop),
    CallsPush,
    CallsPop(Int),
    CallsSet(usize, Int),
    StatusPush,
    StatusPop((V, V, V)),
    StatusSet(usize, (V, V, V)),
    Mmr(u32, Option<V>),
    Mem(u32, u8, u8),
    /// WIDTH bytes at an address that were all in the overlay: their old
    /// value (little-endian).
    MemWord(u32, u8, u32),
}

pub const JOURNAL_CAP: usize = 64;
pub const UNDO_CAP: usize = 64;

pub struct St {
    /// The register file.
    pub r: [V; NUREG],
    /// The current instruction's register writes: code and the value
    /// before the write. From `snap` on they also give the
    /// `_snapshot_uregs` view (a register's first entry there is its value
    /// at the snapshot; one with no entry is unchanged since).
    jr: [u8; JOURNAL_CAP],
    jr_old: [V; JOURNAL_CAP],
    jn: usize,
    snap: usize,
    /// MRF/MRB/MSF/MSB/BFFWRP/BFF_HI/BFF_LO and whether each is present
    /// (a Python dict key).
    pub special: [Spec; 7],
    pub special_present: [bool; 7],
    pub pc_sw: Int,
    pub pending: Option<Pending>,
    pub steps: Int,
    pub at_loaded_entry: bool,
    pub loops: Stk<Loop, MAX_LOOPS>,
    pub call_stack: Stk<Int, MAX_CALLS>,
    pub status_stack: Stk<(V, V, V), MAX_STATUS>,
    /// Fixed-width MMR values, sorted by address.
    pub mmrs: Vec<(u32, V)>,
    pub mem: Mem,
    pub cfg: Cfg,
    undo: [Undo; UNDO_CAP],
    pub un: usize,
    saved: (Int, Option<Pending>, Int, bool),
    /// Instructions completed.
    pub icount: u64,
    /// Block code stops before running past this instruction count.
    pub limit: u64,
    /// The trap that made block code return EXIT_TRAP.
    pub trap: Option<Trap>,
    /// Blocks called directly by other blocks since the dispatcher's call.
    pub chain: u32,
    /// Decoded instruction at a PC (the generated image table).
    pub insn_at: fn(Int) -> Option<&'static Insn>,
    /// Addresses named by sharcimm.name_address: exact addresses and
    /// [lo, hi) ranges, from the image blob.
    pub named_mmrs: Vec<u32>,
    pub named_ranges: Vec<(u32, u32)>,
    pub core_mmr_reset: Vec<u32>,
    /// The image's DO loop end addresses (sorted), and whether every loop
    /// on the stack ends at one of them (block code assumes it).
    pub loop_ends: &'static [i64],
    pub loops_ok: bool,
    /// [lo, hi) windows that hold every named or reset-valued MMR (a quick
    /// test before the exact lookup).
    pub mmr_windows: Vec<(u32, u32)>,
    /// Per 64 KiB page: may hold an MMR (a window above, or the core or
    /// system MMR ranges). Plain-RAM fast paths skip these pages.
    pub mmr_page: Vec<bool>,
}

fn no_insn(_pc: Int) -> Option<&'static Insn> {
    None
}

impl St {
    pub fn new(mem: Mem) -> Box<St> {
        Box::new(St {
            r: [V::UNK; NUREG],
            jr: [0; JOURNAL_CAP],
            jr_old: [V::UNK; JOURNAL_CAP],
            jn: 0,
            snap: 0,
            special: [Spec::default(); 7],
            special_present: [false; 7],
            pc_sw: 0,
            pending: None,
            steps: 0,
            at_loaded_entry: false,
            loops: Stk::default(),
            call_stack: Stk::default(),
            status_stack: Stk::default(),
            mmrs: Vec::new(),
            mem,
            cfg: Cfg::default(),
            undo: [Undo::LoopsPush; UNDO_CAP],
            un: 0,
            saved: (0, None, 0, false),
            icount: 0,
            limit: 0,
            trap: None,
            chain: 0,
            insn_at: no_insn,
            named_mmrs: Vec::new(),
            named_ranges: Vec::new(),
            core_mmr_reset: Vec::new(),
            loop_ends: &[],
            loops_ok: true,
            mmr_windows: Vec::new(),
            mmr_page: vec![false; 1 << 16],
        })
    }

    /// Recompute `mmr_windows` from the named and reset-valued MMRs.
    pub fn set_mmr_windows(&mut self) {
        let mut spans: Vec<(u64, u64)> = Vec::new();
        for &a in self.named_mmrs.iter().chain(self.core_mmr_reset.iter()) {
            spans.push((a as u64, a as u64 + 1));
        }
        for &(lo, hi) in &self.named_ranges {
            spans.push((lo as u64, hi as u64));
        }
        spans.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (lo, hi) in spans {
            match out.last_mut() {
                Some(last) if lo <= last.1 + 0x10_0000 => last.1 = last.1.max(hi),
                _ => out.push((lo, hi)),
            }
        }
        self.mmr_windows = out
            .into_iter()
            .map(|(lo, hi)| (lo as u32, hi.min(u32::MAX as u64) as u32))
            .collect();
        self.mmr_page = vec![false; 1 << 16];
        let mut mark = |lo: u64, hi: u64| {
            if hi > lo {
                for p in (lo >> 16)..=((hi - 1) >> 16).min(0xFFFF) {
                    self.mmr_page[p as usize] = true;
                }
            }
        };
        for &(lo, hi) in &self.mmr_windows.clone() {
            mark(lo as u64, hi as u64);
        }
        // memory.py's CORE_MMR_RANGE and SYSTEM_MMR_RANGE.
        mark(0x30000, 0x32000);
        mark(0x3100_0000, 0x3110_0000);
        self.mem.set_mmr_pages(self.mmr_page.clone());
    }

    /// Recompute `loops_ok` (after an import).
    pub fn check_loops(&mut self) {
        let ends = self.loop_ends;
        self.loops_ok = self
            .loops
            .items()
            .iter()
            .all(|l| ends.binary_search(&l.end_sw).is_ok());
    }

    /// Start an instruction: nothing to undo yet.
    #[inline(always)]
    pub fn begin(&mut self) {
        self.saved = (self.pc_sw, self.pending, self.steps, self.at_loaded_entry);
    }

    /// Finish an instruction: its writes become the next one's snapshot.
    #[inline(always)]
    pub fn commit(&mut self) {
        self.jn = 0;
        self.snap = 0;
        self.un = 0;
        self.icount += 1;
    }

    /// Finish an instruction of block code: its registers are in the
    /// block's register file, so only the undo log is dropped.
    #[inline(always)]
    pub fn commit_blk(&mut self) {
        self.un = 0;
        self.icount += 1;
    }

    /// Keep a host poke's writes (no instruction completed).
    pub fn commit_host(&mut self) {
        self.jn = 0;
        self.snap = 0;
        self.un = 0;
    }

    /// Undo every effect of the current instruction.
    #[cold]
    pub fn rollback(&mut self) {
        for k in (0..self.jn).rev() {
            let c = self.jr[k] as usize;
            self.r[c] = self.jr_old[k];
        }
        self.jn = 0;
        self.snap = 0;
        while self.un > 0 {
            self.un -= 1;
            match self.undo[self.un] {
                Undo::Special(i, v, p) => {
                    self.special[i as usize] = v;
                    self.special_present[i as usize] = p;
                }
                Undo::LoopsPush => self.loops.n -= 1,
                Undo::LoopsPop(v) => {
                    let _ = self.loops.push_raw(v);
                }
                Undo::LoopsSet(i, v) => self.loops.a[i] = v,
                Undo::CallsPush => self.call_stack.n -= 1,
                Undo::CallsPop(v) => {
                    let _ = self.call_stack.push_raw(v);
                }
                Undo::CallsSet(i, v) => self.call_stack.a[i] = v,
                Undo::StatusPush => self.status_stack.n -= 1,
                Undo::StatusPop(v) => {
                    let _ = self.status_stack.push_raw(v);
                }
                Undo::StatusSet(i, v) => self.status_stack.a[i] = v,
                Undo::Mmr(a, old) => match old {
                    Some(v) => self.mmr_put(a, v),
                    None => self.mmrs.retain(|(k, _)| *k != a),
                },
                Undo::Mem(a, byte, flags) => self.mem.restore(a, byte, flags),
                Undo::MemWord(a, w, old) => {
                    self.mem.write_dirty(a, w as u32, old);
                }
            }
        }
        let (pc, pending, steps, entry) = self.saved;
        self.pc_sw = pc;
        self.pending = pending;
        self.steps = steps;
        self.at_loaded_entry = entry;
    }

    /// Undo the current instruction's logged changes (block code: its
    /// registers, PC, pending transfer and step count are restored by the
    /// block itself).
    #[cold]
    pub fn rollback_log(&mut self) {
        let saved = (self.pc_sw, self.pending, self.steps, self.at_loaded_entry);
        self.rollback();
        (self.pc_sw, self.pending, self.steps, self.at_loaded_entry) = saved;
    }

    /// No instruction in progress (after an import).
    pub fn sync_snapshot(&mut self) {
        self.jn = 0;
        self.snap = 0;
        self.un = 0;
    }

    /// Record how to undo a change; a full journal traps (the Python core
    /// runs the instruction).
    #[inline(always)]
    pub fn log(&mut self, u: Undo) -> R<()> {
        if self.un >= UNDO_CAP {
            return Err(TRAP_JOURNAL);
        }
        self.undo[self.un] = u;
        self.un += 1;
        Ok(())
    }

    #[inline(always)]
    pub fn set_r(&mut self, code: usize, v: V) -> R<()> {
        if self.jn >= JOURNAL_CAP {
            return Err(TRAP_JOURNAL);
        }
        self.jr[self.jn] = code as u8;
        self.jr_old[self.jn] = self.r[code];
        self.jn += 1;
        self.r[code] = v;
        Ok(())
    }

    /// Register C in the `_snapshot_uregs` view.
    #[inline(always)]
    pub fn old_of(&self, c: usize) -> V {
        for k in self.snap..self.jn {
            if self.jr[k] as usize == c {
                return self.jr_old[k];
            }
        }
        self.r[c]
    }

    /// `_snapshot_uregs`: the register file as it is now becomes the view
    /// OLD reads.
    #[inline(always)]
    pub fn snapshot(&mut self) {
        self.snap = self.jn;
    }

    pub fn mmr_get(&self, a: u32) -> Option<V> {
        self.mmrs
            .binary_search_by_key(&a, |(k, _)| *k)
            .ok()
            .map(|i| self.mmrs[i].1)
    }

    pub fn mmr_put(&mut self, a: u32, v: V) {
        match self.mmrs.binary_search_by_key(&a, |(k, _)| *k) {
            Ok(i) => self.mmrs[i].1 = v,
            Err(i) => self.mmrs.insert(i, (a, v)),
        }
    }

    fn mmr_set(&mut self, a: u32, v: V) -> R<()> {
        let old = self.mmr_get(a);
        self.log(Undo::Mmr(a, old))?;
        self.mmr_put(a, v);
        Ok(())
    }

    fn special_slot(key: Sym) -> Option<usize> {
        SPECIAL_SLOT_SYMS.iter().position(|&k| k == key)
    }

    #[inline(always)]
    pub fn in_mmr_windows(&self, a: Int) -> bool {
        (0..=u32::MAX as Int).contains(&a)
            && self
                .mmr_windows
                .iter()
                .any(|&(lo, hi)| lo <= a as u32 && (a as u32) < hi)
    }

    pub fn is_named_mmr(&self, a: Int) -> bool {
        if !(0..=u32::MAX as Int).contains(&a) {
            return false;
        }
        let a = a as u32;
        if self.named_mmrs.binary_search(&a).is_ok() {
            return true;
        }
        self.named_ranges.iter().any(|&(lo, hi)| lo <= a && a < hi)
    }
}

// Register, special-register and stack access used by the generated code.

/// A register-file view: the current file, the pre-instruction snapshot,
/// or PEy's view of either (codes 0-15 read S0-S15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegView(pub u8);

impl RegView {
    pub const CUR: RegView = RegView(0);
    pub const OLD: RegView = RegView(1);
    pub const PEY: u8 = 2;
}

/// Block code's register file (tools/sharc_rsgen.py). A block loads the
/// registers it uses from `St` at entry and writes back the ones it wrote
/// at every exit; in between they live here, a local every access of
/// which has a constant index, so LLVM keeps them in host registers. `o`
/// is the instruction's `_snapshot_uregs` view: the register file at the
/// start of the instruction (the block sets the entries it reads).
#[derive(Clone, Copy)]
pub struct Rf {
    pub r: [V; NUREG],
    pub o: [V; NUREG],
    /// state.pc_sw and state.pending.
    pub pc: Int,
    pub pending: Option<Pending>,
}

impl Default for Rf {
    #[inline(always)]
    fn default() -> Self {
        Rf {
            r: [V::UNK; NUREG],
            o: [V::UNK; NUREG],
            pc: 0,
            pending: None,
        }
    }
}

/// rv_get over block code's register file.
#[inline(always)]
pub fn rf_get(rf: &Rf, view: RegView, code: Int) -> V {
    if !(0..NUREG as Int).contains(&code) {
        return V::UNK;
    }
    let c = code as usize;
    if view.0 & RegView::PEY != 0 && c < 16 {
        if view.0 & 1 != 0 {
            rf.o[c + 80]
        } else {
            rf.r[c + 80]
        }
    } else if view.0 & 1 != 0 {
        rf.o[c]
    } else {
        rf.r[c]
    }
}

/// s_set_r over block code's register file, for a value that should be
/// known: anything else leaves block code (a cold exit), so the known-bit
/// masks of the registers stay constants LLVM folds.
#[inline(always)]
pub fn rf_set(rf: &mut Rf, code: Int, v: V) -> R<()> {
    if !(0..NUREG as Int).contains(&code) {
        return Err(TRAP_INDEX);
    }
    if v.m != u32::MAX {
        return Err(TRAP_BLOCK_UNKNOWN);
    }
    rf.r[code as usize] = V {
        b: v.b,
        m: u32::MAX,
    };
    Ok(())
}

/// s_set_r over block code's register file, for a value whose mask is a
/// generation-time constant (V::UNK).
#[inline(always)]
pub fn rf_put(rf: &mut Rf, code: Int, v: V) -> R<()> {
    if !(0..NUREG as Int).contains(&code) {
        return Err(TRAP_INDEX);
    }
    rf.r[code as usize] = v;
    Ok(())
}

#[inline(always)]
pub fn rv_get(s: &St, view: RegView, code: Int) -> V {
    if !(0..NUREG as Int).contains(&code) {
        return V::UNK;
    }
    let mut c = code as usize;
    if view.0 & RegView::PEY != 0 && c < 16 {
        c += 80;
    }
    if view.0 & 1 != 0 { s.old_of(c) } else { s.r[c] }
}

#[inline(always)]
pub fn s_set_r(s: &mut St, code: Int, v: V) -> R<()> {
    if !(0..NUREG as Int).contains(&code) {
        return Err(TRAP_INDEX);
    }
    s.set_r(code as usize, v)
}

/// A special-register view: Python None, the state's own dict, the empty
/// NO_SPECIAL mapping, or `_pey_special` of one of those.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpecView(pub u8);

impl SpecView {
    pub const NONE: SpecView = SpecView(0);
    pub const CUR: SpecView = SpecView(1);
    pub const EMPTY: SpecView = SpecView(2);
    pub const PEY_CUR: SpecView = SpecView(3);
    pub const PEY_EMPTY: SpecView = SpecView(4);
    #[inline(always)]
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
    #[inline(always)]
    pub fn is_some(self) -> bool {
        self.0 != 0
    }
}

#[inline(always)]
pub fn sv_get(s: &St, view: SpecView, key: Sym) -> Option<Spec> {
    match view {
        SpecView::CUR => {
            let i = St::special_slot(key)?;
            if s.special_present[i] {
                Some(s.special[i])
            } else {
                None
            }
        }
        // {"MRF": specials.get("MSF", Unknown)}
        SpecView::PEY_CUR => {
            if key != S_MRF {
                return None;
            }
            let i = St::special_slot(S_MSF).unwrap();
            Some(if s.special_present[i] {
                s.special[i]
            } else {
                Spec::V(V::UNK)
            })
        }
        SpecView::PEY_EMPTY => {
            if key == S_MRF {
                Some(Spec::V(V::UNK))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[inline(always)]
pub fn sv_contains(s: &St, view: SpecView, key: Sym) -> bool {
    sv_get(s, view, key).is_some()
}

#[inline(always)]
pub fn s_set_special(s: &mut St, key: Sym, v: Spec) -> R<()> {
    let i = St::special_slot(key).ok_or(TRAP_KEY)?;
    s.log(Undo::Special(i as u8, s.special[i], s.special_present[i]))?;
    s.special[i] = v;
    s.special_present[i] = true;
    Ok(())
}

macro_rules! stack_ops {
    ($field:ident, $ty:ty, $len:ident, $at:ident, $push:ident, $pop:ident, $set:ident, $tup:ident, $upush:ident, $upop:ident, $uset:ident) => {
        #[inline(always)]
        pub fn $len(s: &St) -> Int {
            s.$field.n as Int
        }
        #[inline(always)]
        pub fn $at(s: &St, i: Int) -> R<$ty> {
            let j = s.$field.index(i)?;
            Ok(s.$field.a[j])
        }
        #[inline(always)]
        pub fn $push(s: &mut St, v: $ty) -> R<()> {
            s.$field.push_raw(v)?;
            s.log(Undo::$upush)
        }
        #[inline(always)]
        pub fn $pop(s: &mut St) -> R<$ty> {
            if s.$field.n == 0 {
                return Err(TRAP_INDEX);
            }
            let v = s.$field.a[s.$field.n - 1];
            s.log(Undo::$upop(v))?;
            s.$field.n -= 1;
            Ok(v)
        }
        #[inline(always)]
        pub fn $set(s: &mut St, i: Int, v: $ty) -> R<()> {
            let j = s.$field.index(i)?;
            s.log(Undo::$uset(j, s.$field.a[j]))?;
            s.$field.a[j] = v;
            Ok(())
        }
        pub fn $tup(s: &St) -> Tup<$ty> {
            Tup::from_slice(s.$field.items())
        }
    };
}

stack_ops!(
    loops,
    Loop,
    stk_len_loops,
    stk_at_loops,
    stk_push_loops,
    stk_pop_loops,
    stk_set_loops,
    stk_tup_loops,
    LoopsPush,
    LoopsPop,
    LoopsSet
);
stack_ops!(
    call_stack,
    Int,
    stk_len_call_stack,
    stk_at_call_stack,
    stk_push_call_stack,
    stk_pop_call_stack,
    stk_set_call_stack,
    stk_tup_call_stack,
    CallsPush,
    CallsPop,
    CallsSet
);
stack_ops!(
    status_stack,
    (V, V, V),
    stk_len_status_stack,
    stk_at_status_stack,
    stk_push_status_stack,
    stk_pop_status_stack,
    stk_set_status_stack,
    stk_tup_status_stack,
    StatusPush,
    StatusPop,
    StatusSet
);

// ---------------------------------------------------------------------------
// Python integer and float semantics
// ---------------------------------------------------------------------------

/// `a << n`: Python raises on a negative count; counts past the native
/// width give the low 128 bits (every caller masks to 80 bits or less).
#[inline(always)]
pub fn shl(a: Int, n: Int) -> R<Int> {
    if n < 0 {
        return Err(TRAP_ARITH);
    }
    Ok(if n >= 128 {
        0
    } else {
        a.wrapping_shl(n as u32)
    })
}

/// `a >> n`, arithmetic like Python's.
#[inline(always)]
pub fn shr(a: Int, n: Int) -> R<Int> {
    if n < 0 {
        return Err(TRAP_ARITH);
    }
    Ok(a >> (n.min(127) as u32))
}

/// Python `//` (floor division).
#[inline(always)]
pub fn floordiv(a: Int, b: Int) -> R<Int> {
    if b == 0 {
        return Err(TRAP_ARITH);
    }
    let q = a / b;
    Ok(if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    })
}

/// Python `%` (sign of the divisor).
#[inline(always)]
pub fn pymod(a: Int, b: Int) -> R<Int> {
    if b == 0 {
        return Err(TRAP_ARITH);
    }
    let r = a % b;
    Ok(if r != 0 && ((r < 0) != (b < 0)) {
        r + b
    } else {
        r
    })
}

#[inline(always)]
pub fn ipow(a: Int, b: Int) -> R<Int> {
    if !(0..128).contains(&b) {
        return Err(TRAP_ARITH);
    }
    Ok(a.wrapping_pow(b as u32))
}

#[inline(always)]
pub fn py_fmod(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r != 0.0 && ((r < 0.0) != (b < 0.0)) {
        r + b
    } else {
        r
    }
}

#[inline(always)]
pub fn bit_length(a: Int) -> Int {
    (128 - a.unsigned_abs().leading_zeros()) as Int
}

#[inline(always)]
pub fn py_min_i(a: Int, b: Int) -> Int {
    if b < a { b } else { a }
}

#[inline(always)]
pub fn py_max_i(a: Int, b: Int) -> Int {
    if b > a { b } else { a }
}

#[inline(always)]
pub fn py_min_f(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

#[inline(always)]
pub fn py_max_f(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

#[inline(always)]
pub fn py_int_of_float(x: f64) -> R<Int> {
    if !x.is_finite() {
        return Err(TRAP_ARITH);
    }
    Ok(x.trunc() as Int)
}

pub struct PyRange {
    cur: Int,
    stop: Int,
    step: Int,
}

impl Iterator for PyRange {
    type Item = Int;
    #[inline(always)]
    fn next(&mut self) -> Option<Int> {
        if (self.step > 0 && self.cur < self.stop) || (self.step < 0 && self.cur > self.stop) {
            let v = self.cur;
            self.cur += self.step;
            Some(v)
        } else {
            None
        }
    }
}

#[inline(always)]
pub fn py_range(start: Int, stop: Int, step: Int) -> R<PyRange> {
    if step == 0 {
        return Err(TRAP_ARITH);
    }
    Ok(PyRange {
        cur: start,
        stop,
        step,
    })
}

#[inline(always)]
pub fn py_range_tup(start: Int, stop: Int, step: Int) -> R<Tup<Int>> {
    let mut t = Tup::new();
    for v in py_range(start, stop, step)? {
        t.push(v)?;
    }
    Ok(t)
}

#[inline(always)]
pub fn f_isnan(x: f64) -> bool {
    x.is_nan()
}
#[inline(always)]
pub fn f_isinf(x: f64) -> bool {
    x.is_infinite()
}
#[inline(always)]
pub fn f_isfinite(x: f64) -> bool {
    x.is_finite()
}
#[inline(always)]
pub fn f_copysign(x: f64, y: f64) -> f64 {
    x.copysign(y)
}
/// Python float arithmetic as this host (aarch64) does it: with a NaN
/// operand the result is that NaN, the first one when both are (FPCR.DN
/// off; the core's doubles are always quiet NaNs, since every f32 reaches
/// them through `_f32_from_bits`). Spelled out because LLVM may commute
/// the operands of `+` and `*`.
#[inline(always)]
pub fn fadd(a: f64, b: f64) -> f64 {
    let r = a + b;
    if r.is_nan() { nan_order(a, b, r) } else { r }
}
#[inline(always)]
pub fn fsub(a: f64, b: f64) -> f64 {
    let r = a - b;
    if r.is_nan() { nan_order(a, b, r) } else { r }
}
#[inline(always)]
pub fn fmul(a: f64, b: f64) -> f64 {
    let r = a * b;
    if r.is_nan() { nan_order(a, b, r) } else { r }
}
/// A NaN result: the first NaN operand, else the one the operation made
/// (inf - inf and the like).
#[cold]
#[inline(never)]
fn nan_order(a: f64, b: f64, r: f64) -> f64 {
    if a.is_nan() {
        a
    } else if b.is_nan() {
        b
    } else {
        r
    }
}

/// Python `/` on floats: ZeroDivisionError on a zero divisor (a trap).
#[inline(always)]
pub fn fdiv(a: f64, b: f64) -> R<f64> {
    if b == 0.0 {
        return Err(TRAP_ARITH);
    }
    let r = a / b;
    Ok(if r.is_nan() { nan_order(a, b, r) } else { r })
}
/// math.sqrt; Python raises on a negative input, the callers never pass one.
#[inline(always)]
pub fn f_sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// C ldexp / scalbn (musl's algorithm): X * 2**N rounded once.
pub fn scalbn(x: f64, n: Int) -> f64 {
    let mut n: i32 = n.clamp(-100_000, 100_000) as i32;
    let x1p1023 = f64::from_bits(0x7fe0_0000_0000_0000);
    let x1p53 = f64::from_bits(0x4340_0000_0000_0000);
    let x1p_1022 = f64::from_bits(0x0010_0000_0000_0000);
    let mut y = x;
    if n > 1023 {
        y *= x1p1023;
        n -= 1023;
        if n > 1023 {
            y *= x1p1023;
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        y *= x1p_1022 * x1p53;
        n += 1022 - 53;
        if n < -1022 {
            y *= x1p_1022 * x1p53;
            n += 1022 - 53;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    y * f64::from_bits(((0x3ff + n) as u64) << 52)
}

pub mod bnd;
