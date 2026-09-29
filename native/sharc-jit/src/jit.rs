//! The desktop JIT: the runtime module (`rt/`, wasm32) under wasmtime, the
//! translator (`translate/`) run on the host when the runtime reports a hot
//! PC, and each translated region compiled into its own module that shares
//! the runtime's memory, helpers and function table.
//!
//! Deterministic by construction: a region is translated at a point fixed
//! by the instruction stream (a run-start counter reaching its threshold),
//! from the image, the interpreter's executed-PC bits and MODE1 at that
//! point; the translator iterates only ordered collections.

use sharc_decode::{Decoder, SegmentImage, ShortWords};
use sharc_translate::pe::mach::{CfgVals, Ctx, DInsn, InsnSource, Layout};
use sharc_translate::pe::region::{self, Limits, Spec, Translator};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::Instant;
use wasmtime::{
    Caller, Config, Engine, Extern, Func, Instance, Linker, Memory, Module, OptLevel, Ref, Store, Table, TypedFunc,
};

/// The runtime module, built for wasm32 from `native/sharc-jit/rt`.
#[cfg(jit_rt)]
pub const RT_WASM: &[u8] = include_bytes!(env!("SHARC_JIT_RT"));
#[cfg(not(jit_rt))]
pub const RT_WASM: &[u8] = &[];

/// Instructions of the image, decoded on demand.
pub struct ImageInsns {
    pub img: SegmentImage,
    dec: Decoder,
    cache: RefCell<HashMap<u32, Option<Rc<DInsn>>>>,
    /// One instruction standing in at a PC (exec_insn's fabricated cases).
    pub overlay: RefCell<Option<(u32, Rc<DInsn>)>>,
}

impl ImageInsns {
    pub fn new(img: SegmentImage) -> ImageInsns {
        ImageInsns {
            img,
            dec: Decoder::new(),
            cache: RefCell::new(HashMap::new()),
            overlay: RefCell::new(None),
        }
    }

    fn decode(&self, pc: u32) -> DInsn {
        let d = self.dec.decode_at(&self.img, pc);
        DInsn {
            type_name: d.type_name,
            kind: d.kind,
            length_bytes: d.length_bytes,
            fields: d.fields,
        }
    }

    /// Every DO loop end in the image (Type12a: pc + signed 23-bit reladdr,
    /// sequencer._start_counted_loop), over every decodable PC: a superset
    /// of the ends a loop on the stack can have.
    pub fn loop_ends(&self) -> Vec<i64> {
        let mut ends = Vec::new();
        for (lo, hi) in self.img.pc_ranges() {
            // The sequencer's PCs are 24-bit short-word addresses.
            let hi = hi.min(1 << 24);
            for pc in lo..hi {
                if !self.dec.may_be(&self.img, pc, &["12a_imm", "12a_ureg"]) {
                    continue;
                }
                let d = self.dec.decode_at(&self.img, pc);
                if (d.type_name == "12a_imm" || d.type_name == "12a_ureg") && d.length_bytes.is_some() {
                    let f = |k: &str| d.fields.iter().find(|(n, _)| n == k).map(|x| x.1);
                    if let (Some(h), Some(l)) = (f("reladdr[22:16]"), f("reladdr[15:0]")) {
                        let raw = (h << 16) | l;
                        let rel = if raw & (1 << 22) != 0 { raw - (1 << 23) } else { raw };
                        ends.push(pc as i64 + rel);
                    }
                }
            }
        }
        ends.sort_unstable();
        ends.dedup();
        ends
    }
}

impl InsnSource for ImageInsns {
    fn insn(&self, pc: u32) -> Option<Rc<DInsn>> {
        if let Some((p, d)) = &*self.overlay.borrow()
            && *p == pc
        {
            return Some(d.clone());
        }
        if let Some(x) = self.cache.borrow().get(&pc) {
            return x.clone();
        }
        let d = self.decode(pc);
        let v = if d.length_bytes.is_some() { Some(Rc::new(d)) } else { None };
        self.cache.borrow_mut().insert(pc, v.clone());
        v
    }
}

/// Translation statistics.
#[derive(Default, Clone, Debug)]
pub struct JitStats {
    pub regions: u64,
    pub refused: u64,
    pub insns: u64,
    pub blocks: u64,
    pub bytes: u64,
    pub translate_ns: u64,
    pub compile_ns: u64,
    pub failures: BTreeMap<String, u64>,
    /// SHA-256 over every module's bytes in order (the determinism check).
    pub modules_sha: [u8; 32],
    hasher: Option<Vec<u8>>,
}

pub struct Shared {
    pub tr: Translator,
    pub insns: ImageInsns,
    pub loop_ends: Rc<Vec<i128>>,
    pub lay: Layout,
    pub cfg: CfgVals,
    pub limits: Limits,
    mem: Option<Memory>,
    table: Option<Table>,
    helpers: Vec<Func>,
    st_ptr: u32,
    seen_ptr: u32,
    pub stats: JitStats,
    pub started: Instant,
    /// Keep every module (their bytes, for inspection).
    pub keep_modules: bool,
    pub modules: Vec<(u32, Vec<u8>)>,
    /// Translate with per-instruction trace calls (debugging).
    pub trace: bool,
}

pub struct Machine {
    pub store: Store<Shared>,
    pub inst: Instance,
    pub mem: Memory,
    f_alloc: TypedFunc<u32, u32>,
    f_free: TypedFunc<(u32, u32), ()>,
    f_step: TypedFunc<u32, i32>,
}

fn engine() -> wasmtime::Result<Engine> {
    let mut c = Config::new();
    c.cranelift_opt_level(OptLevel::Speed);
    // NaN results the same on every host (aarch64's default NaN).
    c.cranelift_nan_canonicalization(true);
    Engine::new(&c)
}

thread_local! {
    static ENGINE: Engine = engine().expect("wasmtime engine");
    static PROGRAM: RefCell<Option<Rc<RuntimeModule>>> = const { RefCell::new(None) };
}

/// The compiled runtime module and the parsed core (shared by machines on
/// one thread).
pub struct RuntimeModule {
    module: Module,
}

fn runtime_module() -> wasmtime::Result<Rc<RuntimeModule>> {
    PROGRAM.with(|p| {
        if let Some(m) = &*p.borrow() {
            return Ok(m.clone());
        }
        if RT_WASM.is_empty() {
            wasmtime::bail!("built without the runtime module (SHARC_JIT_RT)");
        }
        let module = ENGINE.with(|e| Module::new(e, RT_WASM))?;
        let m = Rc::new(RuntimeModule { module });
        *p.borrow_mut() = Some(m.clone());
        Ok(m)
    })
}

/// Parse the transpiled core (a few ms).
pub fn translator() -> Result<Translator, String> {
    Translator::new(sharc_translate::program()?)
}

fn sha256(parts: &[u8]) -> [u8; 32] {
    sharc_native_sha(parts)
}

fn sharc_native_sha(b: &[u8]) -> [u8; 32] {
    let mut h = crate::sha::Sha256::new();
    h.update(b);
    h.finish()
}

impl Machine {
    /// A machine over IMAGE (a pack_image blob).
    pub fn new(image: &[u8]) -> wasmtime::Result<Machine> {
        let verbose = std::env::var("SHARC_JIT_VERBOSE").is_ok();
        let t0 = Instant::now();
        let rtm = runtime_module()?;
        let t1 = Instant::now();
        let tr = translator().map_err(wasmtime::Error::msg)?;
        let t2 = Instant::now();
        let img = sharc_decode::parse_pack_image(image).map_err(wasmtime::Error::msg)?;
        let insns = ImageInsns::new(img);
        let ends = insns.loop_ends();
        if verbose {
            let n: u64 = insns
                .img
                .pc_ranges()
                .iter()
                .map(|r| (r.1.min(1 << 24) as u64).saturating_sub(r.0 as u64))
                .sum();
            eprintln!("sharc-jit: {n} PCs scanned for loop ends");
            eprintln!(
                "sharc-jit: runtime module {:.0} ms, core parse {:.0} ms, loop ends {:.0} ms ({})",
                (t1 - t0).as_secs_f64() * 1e3,
                (t2 - t1).as_secs_f64() * 1e3,
                t2.elapsed().as_secs_f64() * 1e3,
                ends.len()
            );
        }
        let shared = Shared {
            tr,
            insns,
            loop_ends: Rc::new(ends.iter().map(|&x| x as i128).collect()),
            lay: Layout::default(),
            cfg: CfgVals::default(),
            limits: {
                let mut l = Limits::default();
                // Debugging: one-block regions of at most N instructions.
                if let Ok(n) = std::env::var("SHARC_JIT_MAX_BLOCK")
                    && let Ok(n) = n.parse::<usize>()
                {
                    l.max_block_insns = n;
                    l.max_blocks = 1;
                }
                l
            },
            mem: None,
            table: None,
            helpers: Vec::new(),
            st_ptr: 0,
            seen_ptr: 0,
            stats: JitStats::default(),
            started: Instant::now(),
            keep_modules: false,
            modules: Vec::new(),
            trace: std::env::var("SHARC_JIT_TRACE").is_ok(),
        };
        let mut store = ENGINE.with(|e| Store::new(e, shared));
        let mut linker: Linker<Shared> = ENGINE.with(|e| Linker::new(e));
        linker.func_wrap("host", "now_ns", |c: Caller<'_, Shared>| c.data().started.elapsed().as_nanos() as f64)?;
        linker.func_wrap("host", "jit", |mut c: Caller<'_, Shared>, pc: u32| -> i32 {
            match translate_install(&mut c, pc, None) {
                Ok(slot) => slot as i32,
                Err(_) => -1,
            }
        })?;
        let inst = linker.instantiate(&mut store, &rtm.module)?;
        let mem = inst.get_memory(&mut store, "memory").ok_or_else(|| wasmtime::Error::msg("no memory"))?;
        let table = inst
            .get_table(&mut store, "__indirect_function_table")
            .ok_or_else(|| wasmtime::Error::msg("no table export"))?;
        let mut helpers = Vec::new();
        for (name, _, _) in sharc_translate::pe::mach::HELPERS {
            helpers.push(inst.get_func(&mut store, name).ok_or_else(|| wasmtime::Error::msg(format!("no {name}")))?);
        }
        let f_alloc: TypedFunc<u32, u32> = inst.get_typed_func(&mut store, "jit_alloc")?;
        let f_free: TypedFunc<(u32, u32), ()> = inst.get_typed_func(&mut store, "jit_free")?;
        let f_step: TypedFunc<u32, i32> = inst.get_typed_func(&mut store, "jit_step")?;
        let mut m = Machine {
            store,
            inst,
            mem,
            f_alloc,
            f_free,
            f_step,
        };
        let p = m.put(image)?;
        let create: TypedFunc<(u32, u32), i32> = m.inst.get_typed_func(&mut m.store, "jit_create")?;
        let rc = create.call(&mut m.store, (p, image.len() as u32))?;
        m.free(p, image.len())?;
        if rc != 0 {
            wasmtime::bail!("jit_create failed ({rc})");
        }
        // Layout, state pointers, loop ends.
        let lp = m.alloc(4 * Layout::FIELDS)?;
        let layout: TypedFunc<u32, ()> = m.inst.get_typed_func(&mut m.store, "jit_layout")?;
        layout.call(&mut m.store, lp)?;
        let words: Vec<u32> = (0..Layout::FIELDS).map(|k| m.u32_at(lp + 4 * k as u32)).collect();
        m.free(lp, 4 * Layout::FIELDS)?;
        let st: TypedFunc<(), u32> = m.inst.get_typed_func(&mut m.store, "jit_st")?;
        let seen: TypedFunc<(), u32> = m.inst.get_typed_func(&mut m.store, "jit_seen")?;
        let st_ptr = st.call(&mut m.store, ())?;
        let seen_ptr = seen.call(&mut m.store, ())?;
        let ends: Vec<i64> = m.store.data().loop_ends.iter().map(|&x| x as i64).collect();
        let bytes: Vec<u8> = ends.iter().flat_map(|x| x.to_le_bytes()).collect();
        let ep = m.put(&bytes)?;
        let set_ends: TypedFunc<(u32, u32), ()> = m.inst.get_typed_func(&mut m.store, "jit_set_loop_ends")?;
        set_ends.call(&mut m.store, (ep, ends.len() as u32))?;
        let d = m.store.data_mut();
        d.lay = Layout::from_words(&words);
        if std::env::var("SHARC_JIT_VERBOSE").is_ok() {
            eprintln!("sharc-jit: layout {:?}", d.lay);
        }
        d.mem = Some(mem);
        d.table = Some(table);
        d.helpers = helpers;
        d.st_ptr = st_ptr;
        d.seen_ptr = seen_ptr;
        Ok(m)
    }

    pub fn alloc(&mut self, n: usize) -> wasmtime::Result<u32> {
        self.f_alloc.call(&mut self.store, n as u32)
    }

    pub fn free(&mut self, p: u32, n: usize) -> wasmtime::Result<()> {
        self.f_free.call(&mut self.store, (p, n as u32))
    }

    /// Copy B into a fresh buffer in the instance.
    pub fn put(&mut self, b: &[u8]) -> wasmtime::Result<u32> {
        let p = self.alloc(b.len())?;
        self.mem.data_mut(&mut self.store)[p as usize..p as usize + b.len()].copy_from_slice(b);
        Ok(p)
    }

    pub fn bytes(&self, p: u32, n: usize) -> Vec<u8> {
        self.mem.data(&self.store)[p as usize..p as usize + n].to_vec()
    }

    pub fn u32_at(&self, p: u32) -> u32 {
        let d = self.mem.data(&self.store);
        u32::from_le_bytes(d[p as usize..p as usize + 4].try_into().unwrap())
    }

    pub fn func<P: wasmtime::WasmParams, R: wasmtime::WasmResults>(&mut self, name: &str) -> wasmtime::Result<TypedFunc<P, R>> {
        self.inst.get_typed_func(&mut self.store, name)
    }

    pub fn step(&mut self, n: u32) -> wasmtime::Result<i32> {
        self.f_step.call(&mut self.store, n)
    }

    /// Call a function returning bytes into an OUT buffer (grown on need).
    pub fn out_call(&mut self, name: &str, cap0: usize) -> wasmtime::Result<Vec<u8>> {
        let f: TypedFunc<(u32, u32), i32> = self.func(name)?;
        let mut cap = cap0;
        loop {
            let p = self.alloc(cap)?;
            let n = f.call(&mut self.store, (p, cap as u32))?;
            if n >= 0 {
                let v = self.bytes(p, n as usize);
                self.free(p, cap)?;
                return Ok(v);
            }
            self.free(p, cap)?;
            cap = (-n) as usize;
        }
    }

    /// Call a function taking one blob (ptr, len) -> i32.
    pub fn in_call(&mut self, name: &str, b: &[u8]) -> wasmtime::Result<i32> {
        let f: TypedFunc<(u32, u32), i32> = self.func(name)?;
        let p = self.put(b)?;
        let r = f.call(&mut self.store, (p, b.len() as u32))?;
        self.free(p, b.len())?;
        Ok(r)
    }

    pub fn configure(&mut self, threshold: u32, enabled: bool) -> wasmtime::Result<()> {
        let f: TypedFunc<(u32, i32), ()> = self.func("jit_configure")?;
        f.call(&mut self.store, (threshold, enabled as i32))
    }

    /// Translate and install the region at PC now; its slot.
    pub fn translate_now(&mut self, pc: u32, one: Option<Rc<DInsn>>) -> wasmtime::Result<u32> {
        let mut ctx = self.store.as_context_mut();
        let _ = &mut ctx;
        translate_in_store(&mut self.store, pc, one)
    }
}

use wasmtime::AsContextMut;

/// The host import `jit(pc)`.
fn translate_install(c: &mut Caller<'_, Shared>, pc: u32, one: Option<Rc<DInsn>>) -> wasmtime::Result<u32> {
    translate_in(c, pc, one)
}

fn translate_in_store(s: &mut Store<Shared>, pc: u32, one: Option<Rc<DInsn>>) -> wasmtime::Result<u32> {
    translate_in(s, pc, one)
}

/// Translate the region at PC from the current machine state, compile it
/// and put it in the runtime's table.
fn translate_in<C: AsContextMut<Data = Shared>>(c: &mut C, pc: u32, one: Option<Rc<DInsn>>) -> wasmtime::Result<u32> {
    // Debugging: translate only the listed PCs (hex, comma-separated).
    if one.is_none()
        && let Ok(only) = std::env::var("SHARC_JIT_ONLY")
        && !only.split(',').any(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16) == Ok(pc))
    {
        wasmtime::bail!("not in SHARC_JIT_ONLY");
    }
    let t0 = Instant::now();
    let (wasm, info) = {
        let ctx = c.as_context();
        let d: &Shared = ctx.data();
        let mem = d.mem.expect("memory");
        let data = mem.data(&ctx);
        let lay = &d.lay;
        let rb = (d.st_ptr + lay.r + 114 * lay.vsize) as usize;
        let rd = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let (mb, mm) = (rd(rb + lay.vb as usize), rd(rb + lay.vm as usize));
        let mode1 = (mm == u32::MAX).then_some(mb);
        let seen_base = d.seen_ptr as usize;
        let seen = |p: u32| -> bool {
            p < (1 << 24) && data[seen_base + (p >> 3) as usize] & (1 << (p & 7)) != 0
        };
        let mut limits = d.limits.clone();
        if one.is_some() {
            limits.max_block_insns = 1;
            limits.max_blocks = 1;
        }
        *d.insns.overlay.borrow_mut() = one.clone().map(|x| (pc, x));
        let cx = Ctx {
            trace: d.trace,
            cfg: d.cfg.clone(),
            lay: lay.clone(),
            loop_ends: d.loop_ends.clone(),
            insns: &d.insns,
            syms: d.tr.syms.clone(),
            fresh: RefCell::new(BTreeMap::new()),
        };
        let no = |_: u32| false;
        let spec = Spec {
            entry: pc,
            mode1,
            seen: if one.is_some() { &no } else { &seen },
            limits,
        };
        let r = region::translate(&d.tr, &cx, &spec);
        *d.insns.overlay.borrow_mut() = None;
        if let (Ok(r), Ok(dir)) = (&r, std::env::var("SHARC_JIT_DUMP")) {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(format!("{dir}/{pc:06x}.txt"), &r.text);
        }
        match r {
            Ok(r) => (r.wasm.clone(), Ok((r.insns, r.blocks.len(), r.failures))),
            Err(e) => (Vec::new(), Err(e)),
        }
    };
    let t1 = Instant::now();
    let (insns, blocks, failures) = match info {
        Ok(x) => x,
        Err(e) => {
            let mut cx = c.as_context_mut();
            let st = &mut cx.data_mut().stats;
            st.refused += 1;
            *st.failures.entry(format!("region: {e}")).or_default() += 1;
            wasmtime::bail!(e);
        }
    };
    {
        let mut cx = c.as_context_mut();
        let st = &mut cx.data_mut().stats;
        st.translate_ns += (t1 - t0).as_nanos() as u64;
        if std::env::var("SHARC_JIT_DEBUG").is_ok() {
            for (fpc, why) in &failures {
                eprintln!("sharc-jit: {fpc:#x}: {why}");
            }
        }
        for (_, why) in &failures {
            let key: String = why.split(" <- ").next().unwrap_or("").chars().take(120).collect();
            *st.failures.entry(key).or_default() += 1;
        }
    }
    if insns == 0 {
        let mut cx = c.as_context_mut();
        cx.data_mut().stats.refused += 1;
        wasmtime::bail!("empty region");
    }
    let engine = c.as_context().engine().clone();
    let module = Module::new(&engine, &wasm)?;
    let t2 = Instant::now();
    let (mem, table, helpers) = {
        let d = c.as_context().data();
        (d.mem.unwrap(), d.table.unwrap(), d.helpers.clone())
    };
    let mut imports: Vec<Extern> = vec![Extern::Memory(mem)];
    imports.extend(helpers.into_iter().map(Extern::Func));
    let inst = Instance::new(&mut *c, &module, &imports)?;
    let f = inst.get_func(&mut *c, "r").ok_or_else(|| wasmtime::Error::msg("no r"))?;
    let slot = table.grow(&mut *c, 1, Ref::Func(Some(f)))?;
    let mut cx = c.as_context_mut();
    let d = cx.data_mut();
    let st = &mut d.stats;
    st.regions += 1;
    st.insns += insns as u64;
    st.blocks += blocks as u64;
    st.bytes += wasm.len() as u64;
    st.compile_ns += (t2 - t1).as_nanos() as u64;
    let mut buf = st.modules_sha.to_vec();
    buf.extend_from_slice(&wasm);
    st.modules_sha = sha256(&buf);
    let _ = &st.hasher;
    if d.keep_modules {
        d.modules.push((pc, wasm));
    }
    Ok(slot as u32)
}
