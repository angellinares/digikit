//! ColdFire C and assembly objects: compile, merge, resolve the compiler helpers, read the ELF, link at a placeholder base,
//! check every instruction. gcc, as, ld, objcopy and nm are child processes with the arguments the Python builder used.

use super::elf::{self, PSection, PSymbol, Proto, R_68K, RelocTo};
use super::toolchain::{self as tc, Env, TempDir};
use crate::dko::{Dko, DkoReloc, DkoSection, DkoSymbol, FORMAT, ISA_CHECK_KEY};
use crate::error::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Where the check links an object: the module area of the runtime. Placeholder externs follow at `BASE + 0x100 + 8 * i`.
pub const BASE: u32 = 0x47E8B000;
const KIND_ORDER: [&str; 4] = ["text", "rodata", "data", "bss"];
/// Privileged forms a module may use: cache line push-and-invalidate, and the status register moves.
const PRIVILEGED_OK: [&str; 3] = ["cpushl", "move_from_sr", "move_to_sr"];
const ALLOWED_UNITS: [&str; 4] = ["isa_a", "isa_b", "isa_c", "emac"];

fn is_asm(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()), Some("s" | "S"))
}

fn is_c_or_asm(p: &Path) -> bool {
    is_asm(p) || p.extension().is_some_and(|e| e == "c")
}

fn inc_flags(dirs: &[PathBuf]) -> Vec<String> {
    dirs.iter().map(|d| format!("-I{}", d.display())).collect()
}

/// `-DNAME` for each build define (C sources only: the assembler's `-D` means something else).
fn def_flags(env: &Env) -> Vec<String> {
    env.defines.iter().map(|d| format!("-D{d}")).collect()
}

/// One C or assembly file to a relocatable ELF.
pub fn compile_source(env: &Env, path: &Path, out: &Path, include_dirs: &[PathBuf]) -> Result<()> {
    let (prog, mut args) = if is_asm(path) {
        (env.m68k("as")?, tc::strs(&tc::ASFLAGS))
    } else {
        (env.m68k("gcc")?, tc::strs(&tc::CFLAGS))
    };
    args.extend(inc_flags(include_dirs));
    if !is_asm(path) {
        args.extend(def_flags(env));
        args.push("-c".into());
    }
    args.extend([
        "-o".into(),
        out.display().to_string(),
        path.display().to_string(),
    ]);
    tc::run(&prog, &args, None).map_err(|e| {
        err!(
            "{} (on {})",
            e.0.replacen(" failed:", " failed", 1),
            path.display()
        )
    })?;
    Ok(())
}

/// Combine several relocatable ELFs into one (`ld -r`), keeping relocations.
pub fn merge_objects(env: &Env, objs: &[PathBuf], out: &Path) -> Result<()> {
    let mut args = tc::strs(&["-r", "-o"]);
    args.push(out.display().to_string());
    args.extend(objs.iter().map(|o| o.display().to_string()));
    tc::run(&env.m68k("ld")?, &args, None)?;
    Ok(())
}

/// The names `nm` lists for `obj` (the last word of each line).
fn nm(env: &Env, obj: &Path, flags: &[&str]) -> Result<Vec<String>> {
    let mut args = tc::strs(flags);
    args.push(obj.display().to_string());
    let r = tc::run(&env.m68k("nm")?, &args, None)?;
    Ok(r.stdout
        .lines()
        .filter_map(|l| l.split_whitespace().last().map(String::from))
        .collect())
}

/// Link the compiler helpers an object leaves undefined (`__udivdi3`, `__ashldi3`, ...) from the ColdFire libgcc
/// (`libgcc-cf54418.a`, built by `pk_toolchains libgcc-cf`): `ld -r` searches the archive for the undefined symbols only, and every
/// symbol the pulled members define is made local, so two modules that both carry `__udivdi3` do not collide. Their `.eh_frame` is
/// removed. Returns false (and writes nothing) when the object has no helper to resolve.
pub fn resolve_helpers(env: &Env, obj: &Path, out: &Path) -> Result<bool> {
    let undefined = nm(env, obj, &["-u"])?;
    if undefined.is_empty() {
        return Ok(false);
    }
    let Some(lib) = env.libgcc_cf() else {
        return Ok(false);
    };
    let helpers: HashSet<String> = nm(env, &lib, &["-g", "--defined-only"])?
        .into_iter()
        .filter(|n| n.starts_with("__"))
        .collect();
    if !undefined.iter().any(|n| helpers.contains(n)) {
        return Ok(false);
    }
    let before: BTreeSet<String> = nm(env, obj, &["-g", "--defined-only"])?
        .into_iter()
        .collect();
    let args = vec![
        "-r".to_string(),
        "-o".into(),
        out.display().to_string(),
        obj.display().to_string(),
        lib.display().to_string(),
    ];
    tc::run(&env.m68k("ld")?, &args, None)
        .map_err(|e| err!("ld -r with {} {}", tc::LIBGCC_CF_NAME, e.0))?;
    let after: BTreeSet<String> = nm(env, out, &["-g", "--defined-only"])?
        .into_iter()
        .collect();
    let mut args = vec!["--remove-section=.eh_frame".to_string()];
    args.extend(
        after
            .difference(&before)
            .map(|n| format!("--localize-symbol={n}")),
    );
    args.push(out.display().to_string());
    tc::run(&env.m68k("objcopy")?, &args, None)?;
    Ok(true)
}

/// Tokens of a make dependency rule after the first `: `: split at white space not preceded by a backslash, `\ ` unescaped.
pub fn parse_make_deps(text: &str) -> Vec<String> {
    let body = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let rest = body.split_once(": ").map(|x| x.1).unwrap_or("").trim();
    let chars: Vec<char> = rest.chars().collect();
    let mut out = vec![];
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let prev_bs = i > 0 && chars[i - 1] == '\\';
        if chars[i].is_whitespace() && !prev_bs {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(chars[i]);
            i += 1;
        }
    }
    out.push(cur);
    out.into_iter()
        .filter(|t| !t.is_empty())
        .map(|t| t.replace("\\ ", " "))
        .collect()
}

/// A path relative to the repository root as text, else the path itself.
pub fn rel(env: &Env, p: &Path) -> String {
    match p.strip_prefix(&env.root) {
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

/// The files a source reads besides itself (headers, `.include` files), resolved and sorted by repository-relative path.
pub fn deps(env: &Env, src: &Path, include_dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let tmp = TempDir::new("pk-deps-")?;
    let text = if is_asm(src) {
        let mf = tmp.path().join("deps.d");
        let mut args = tc::strs(&tc::ASFLAGS);
        args.extend(inc_flags(include_dirs));
        args.extend([
            "--MD".into(),
            mf.display().to_string(),
            "-o".into(),
            tmp.path().join("x.o").display().to_string(),
            src.display().to_string(),
        ]);
        let as_ = env.m68k("as")?;
        tc::run(&as_, &args, None).map_err(|e| {
            err!(
                "as failed listing dependencies of {}:\n{}",
                src.display(),
                e.0
            )
        })?;
        std::fs::read_to_string(&mf).unwrap_or_default()
    } else {
        let mut args = tc::strs(&tc::CFLAGS);
        args.extend(inc_flags(include_dirs));
        args.extend(def_flags(env));
        args.extend([
            "-MM".into(),
            "-MT".into(),
            "x".into(),
            src.display().to_string(),
        ]);
        let gcc = env.m68k("gcc")?;
        tc::run(&gcc, &args, None)
            .map_err(|e| {
                err!(
                    "gcc failed listing dependencies of {}:\n{}",
                    src.display(),
                    e.0
                )
            })?
            .stdout
    };
    let me = tc::canon(src);
    let found: BTreeSet<PathBuf> = parse_make_deps(&text)
        .iter()
        .map(|t| tc::canon(Path::new(t)))
        .filter(|p| *p != me)
        .collect();
    let mut v: Vec<PathBuf> = found.into_iter().collect();
    v.sort_by_key(|p| rel(env, p));
    Ok(v)
}

/// The `source_sha256` of a ColdFire object: the sources, then every header and `.include` file they read (path and bytes), then the
/// toolchain record.
pub fn source_digest(env: &Env, sources: &[PathBuf], include_dirs: &[PathBuf]) -> Result<String> {
    let mut h = Sha256::new();
    for p in sources {
        h.update(tc::read(p)?);
    }
    if sources.iter().any(|p| is_c_or_asm(p)) {
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        for p in sources.iter().filter(|p| is_c_or_asm(p)) {
            seen.extend(deps(env, p, include_dirs)?);
        }
        let mut v: Vec<PathBuf> = seen.into_iter().collect();
        v.sort_by_key(|p| rel(env, p));
        for d in v {
            h.update(rel(env, &d).as_bytes());
            h.update([0u8]);
            h.update(tc::read(&d)?);
        }
        h.update(env.toolchain_id()?.as_bytes());
        // the define set is part of the key; an empty set adds nothing, so the hashes of untraced builds stay as they were
        for d in &env.defines {
            h.update(b"\0-D");
            h.update(d.as_bytes());
        }
    }
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    crate::error::hex(b)
}

/// The prototype of C and assembly sources: compile each, merge, add the compiler helpers.
pub fn proto_of_sources(env: &Env, sources: &[PathBuf], include_dirs: &[PathBuf]) -> Result<Proto> {
    let tmp = TempDir::new("pk-build-")?;
    let mut objs = vec![];
    for (i, src) in sources.iter().enumerate() {
        let stem = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let o = tmp.path().join(format!("{i}_{stem}.o"));
        compile_source(env, src, &o, include_dirs)?;
        objs.push(o);
    }
    let mut merged = objs[0].clone();
    if objs.len() > 1 {
        merged = tmp.path().join("all.o");
        merge_objects(env, &objs, &merged)?;
    }
    let with_helpers = tmp.path().join("helpers.o");
    if resolve_helpers(env, &merged, &with_helpers)? {
        merged = with_helpers; // __udivdi3 and friends, from the ColdFire libgcc
    }
    elf::proto_from_elf(&tc::read(&merged)?)
}

/// A prototype placed at an address.
pub struct Linked {
    pub base: u32,
    /// text, rodata and data at `base`; bss (zeros) follows
    pub image: Vec<u8>,
    pub bss_size: u32,
    pub symbols: HashMap<String, u32>,
    pub text_ranges: Vec<(u32, u32)>,
    pub placement: HashMap<String, u32>,
    pub relocs_applied: BTreeMap<String, usize>,
}

/// Place the sections at `base` (text, rodata, data, then bss), resolve the undefined symbols from `symmap` and apply the relocations.
pub fn link(proto: &Proto, base: u32, symmap: &HashMap<String, u32>) -> Result<Linked> {
    let mut secs: Vec<&PSection> = proto.sections.iter().collect();
    secs.sort_by_key(|s| KIND_ORDER.iter().position(|k| *k == s.kind).unwrap_or(9));
    let mut placement: HashMap<String, u32> = HashMap::new();
    let mut addr = base as u64;
    let mut image: Vec<u8> = vec![];
    let mut text_ranges = vec![];
    let mut bss_start: Option<u64> = None;
    for s in &secs {
        let al = s.align.max(1) as u64;
        addr = addr.div_ceil(al) * al;
        placement.insert(s.name.clone(), addr as u32);
        if s.kind == "bss" {
            bss_start.get_or_insert(addr);
        } else {
            let want = (addr - base as u64) as usize;
            if image.len() < want {
                image.resize(want, 0);
            }
            image.extend_from_slice(&s.data);
            if s.kind == "text" {
                text_ranges.push((addr as u32, (addr + s.size as u64) as u32));
            }
        }
        addr += s.size as u64;
    }
    let bss_size = bss_start.map(|b| (addr - b) as u32).unwrap_or(0);
    let mut syms: HashMap<String, u32> = HashMap::new();
    for sy in &proto.symbols {
        if let Some(sec) = &sy.section {
            syms.insert(
                sy.name.clone(),
                placement[sec].wrapping_add(sy.value as u32),
            );
        }
    }
    for sy in &proto.symbols {
        if sy.section.is_none() && !syms.contains_key(&sy.name) {
            let Some(v) = symmap.get(&sy.name) else {
                return Err(err!("unresolved extern symbol {}", sy.name));
            };
            syms.insert(sy.name.clone(), *v);
        }
    }
    let mut applied: BTreeMap<String, usize> = BTreeMap::new();
    for r in &proto.relocs {
        let (to_name, s_val) = match &r.to {
            RelocTo::Symbol(n) => match syms.get(n) {
                Some(v) => (n.as_str(), *v as i64),
                None => return Err(err!("relocation against unknown symbol {n}")),
            },
            RelocTo::Section(n) => (n.as_str(), placement[n] as i64),
        };
        let place = placement[&r.section] as i64 + r.offset as i64;
        let mut value = s_val + r.addend;
        let &(_, _, size, pcrel) = R_68K.iter().find(|x| x.1 == r.rtype).expect("known type");
        let bits = 8 * size as u32;
        let (lo, hi) = if pcrel {
            value -= place;
            (-(1i64 << (bits - 1)), 1i64 << (bits - 1))
        } else {
            (-(1i64 << (bits - 1)), 1i64 << bits)
        };
        if !(lo <= value && value < hi) {
            return Err(err!(
                "relocation {} at {place:#x} -> {to_name}: {value:#x} out of range",
                r.rtype
            ));
        }
        let off = (place - base as i64) as usize;
        let bytes = (value as u64 & ((1u64 << bits) - 1)).to_be_bytes();
        let Some(dst) = image.get_mut(off..off + size) else {
            return Err(err!(
                "relocation {} at {place:#x} is outside the image",
                r.rtype
            ));
        };
        dst.copy_from_slice(&bytes[8 - size..]);
        *applied.entry(r.rtype.to_string()).or_default() += 1;
    }
    Ok(Linked {
        base,
        image,
        bss_size,
        symbols: syms,
        text_ranges,
        placement,
        relocs_applied: applied,
    })
}

/// Decode every instruction of the executable sections with our own decoder (the `coldfire` crate, generated from
/// tools/coldfire/cfisa/coldfire.json); returns the form histogram. Anything that is not ISA_A/B/C or EMAC (a 68020+ form, an FPU form,
/// a privileged form other than the ones a module may use, an illegal word) is an error listing every such instruction.
pub fn check_isa(l: &Linked) -> Result<BTreeMap<String, u32>> {
    check_ranges(&l.image, l.base, &l.text_ranges, &|a| format!("{a:08x}"))
}

/// [`check_isa`] over code ranges `(lo, hi)` of `image` loaded at `base`; `at` names an address in the error.
fn check_ranges(
    image: &[u8],
    base: u32,
    ranges: &[(u32, u32)],
    at: &dyn Fn(u32) -> String,
) -> Result<BTreeMap<String, u32>> {
    let mut hist: BTreeMap<String, u32> = BTreeMap::new();
    let mut bad: Vec<String> = vec![];
    for &(lo, hi) in ranges {
        let mut addr = lo;
        while addr < hi {
            let off = addr.wrapping_sub(base) as usize;
            match coldfire::decode_at(image, base, off) {
                Some(i) => {
                    let form = i.id();
                    *hist.entry(form.to_string()).or_default() += 1;
                    let unit = i.unit();
                    let priv_ = i.privileged();
                    if !ALLOWED_UNITS.contains(&unit) || (priv_ && !PRIVILEGED_OK.contains(&form)) {
                        bad.push(format!(
                            "{}: {form} ({unit}{})",
                            at(addr),
                            if priv_ { ", privileged" } else { "" }
                        ));
                    }
                    addr += (i.len as u32).max(2);
                }
                None => {
                    bad.push(format!("{}: illegal encoding", at(addr)));
                    addr += 2;
                }
            }
        }
    }
    if !bad.is_empty() {
        return Err(err!(
            "instructions outside ISA_A/B/C+EMAC:\n  {}",
            bad.join("\n  ")
        ));
    }
    Ok(hist)
}

/// [`check_isa`] over code blobs `(name, bytes)` that are not linked (the text sections of an object or of the members of an archive).
/// `label` is put before each section name in the error.
pub fn check_text(sections: &[(String, Vec<u8>)], label: &str) -> Result<BTreeMap<String, u32>> {
    let mut image: Vec<u8> = vec![];
    let mut ranges = vec![];
    let mut starts: Vec<(u32, &str)> = vec![];
    for (name, blob) in sections {
        while image.len() % 4 != 0 {
            image.push(0);
        }
        let lo = image.len() as u32;
        image.extend_from_slice(blob);
        ranges.push((lo, image.len() as u32));
        starts.push((lo, name));
    }
    let at = |a: u32| {
        let (lo, name) = starts
            .iter()
            .rev()
            .find(|s| s.0 <= a)
            .copied()
            .unwrap_or((0, "?"));
        format!("{label}{name}+{:#x}", a - lo)
    };
    check_ranges(&image, 0, &ranges, &at)
}

/// The object a finished prototype becomes: linked once at `BASE` with placeholder externs (so PC16 and PC8 calls reach), checked, and
/// stamped with the ISA record `patchkit pack` verifies.
pub fn dko_from_proto(
    proto: &Proto,
    source_sha256: String,
    mut toolchain: BTreeMap<String, String>,
) -> Result<Dko> {
    let undefined: Vec<&str> = proto
        .symbols
        .iter()
        .filter(|s| s.section.is_none())
        .map(|s| s.name.as_str())
        .collect();
    let symmap: HashMap<String, u32> = undefined
        .iter()
        .enumerate()
        .map(|(i, n)| (n.to_string(), BASE + 0x100 + 8 * i as u32))
        .collect();
    let linked = link(proto, BASE, &symmap)?;
    let hist = check_isa(&linked)?;
    let sections: Vec<DkoSection> = proto
        .sections
        .iter()
        .map(|s| DkoSection {
            name: s.name.clone(),
            kind: match s.kind {
                "rodata" => "data".into(),
                k => k.into(),
            },
            align: s.align,
            size: s.size,
            hex: hex(&s.data),
        })
        .collect();
    let mut symbols: Vec<DkoSymbol> = proto
        .symbols
        .iter()
        .map(|s: &PSymbol| DkoSymbol {
            name: s.name.clone(),
            section: s.section.clone(),
            value: s.value,
            global: s.global,
        })
        .collect();
    let mut seen: HashSet<String> = symbols.iter().map(|s| s.name.clone()).collect();
    let mut relocs = vec![];
    for r in &proto.relocs {
        let t = match r.rtype {
            "PLT32" => "PC32",
            "PLT16" => "PC16",
            "PLT8" => "PC8",
            t => t,
        };
        if !matches!(t, "32" | "PC32" | "PC16" | "PC8") {
            return Err(err!(
                "relocation type {} in {} is not supported (use 32-bit absolute or PC-relative calls)",
                r.rtype,
                r.section
            ));
        }
        let sym = match &r.to {
            RelocTo::Section(sec) => {
                let name = format!(".sec.{sec}");
                if seen.insert(name.clone()) {
                    symbols.push(DkoSymbol {
                        name: name.clone(),
                        section: Some(sec.clone()),
                        value: 0,
                        global: false,
                    });
                }
                name
            }
            RelocTo::Symbol(n) => n.clone(),
        };
        relocs.push(DkoReloc {
            section: r.section.clone(),
            offset: r.offset,
            rtype: match t {
                "32" => "cf.32",
                "PC32" => "cf.pc32",
                "PC16" => "cf.pc16",
                _ => "cf.pc8",
            }
            .into(),
            symbol: sym,
            addend: r.addend,
        });
    }
    symbols.sort_by(|a, b| a.name.cmp(&b.name));
    toolchain.insert(
        "instructions_redecoded".into(),
        hist.values().sum::<u32>().to_string(),
    );
    let mut d = Dko {
        format: FORMAT,
        arch: "cf".into(),
        source_sha256,
        toolchain,
        sections,
        symbols,
        relocs,
    };
    // the check passed above: record it over these bytes, for `patchkit pack`
    let digest = d.isa_digest()?;
    d.toolchain.insert(ISA_CHECK_KEY.into(), digest);
    Ok(d)
}

/// An object from C and assembly sources.
pub fn cf_object(env: &Env, sources: &[PathBuf], include_dirs: &[PathBuf]) -> Result<Dko> {
    let proto = proto_of_sources(env, sources, include_dirs)?;
    let source_sha256 = source_digest(env, sources, include_dirs)?;
    let toolchain: BTreeMap<String, String> = env.toolchain_version()?.into_iter().collect();
    dko_from_proto(&proto, source_sha256, toolchain)
}
