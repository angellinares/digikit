//! `patchkit bind`: a service `.dks` is packed without firmware. Binding adds, for each hook the service declares (the `HOOKS` entry of its
//! header table), a trampoline to the image and a `BIND` item, using the user's firmware image. The trampoline saves the registers the
//! hook may not clobber, calls the service's handler with the arguments the hook declares, restores, runs the displaced instructions
//! and jumps back. The bound file holds firmware bytes (the stock bytes of each site): it is never committed.

use crate::apply::{HookSite, resolve_hook_site};
use crate::dkm::{Dkm, EmitParts, FLAG_TRACE, ROLE_SERVICE, emit, max_file};
use crate::error::*;
use crate::firmware::Firmware;
use crate::fwgen::{HandlerArg, HandlerPlan, handler_plan};
use crate::port::Registry;
use crate::prim::{self, CF_JMP_L, CfSite, cf_tail};
use dkneeds::keys::{
    BIND_STOCK_MAX, Bind, KeyGroup, Keys, Pair, Table, name_hash, table_size, write_table,
};
use std::path::{Path, PathBuf};

/// Most `BIND` items of one service. A patchkit constant, not an `sdk.toml` limit: no runtime array is sized with it (the runtime reads the
/// items from the table).
pub const BIND_MAX: usize = 24;

/// One bound hook, for the report.
#[derive(Clone, Debug)]
pub struct BoundHook {
    pub hook: String,
    /// `name_hash` of the hook name: the `site` of the `BIND` item.
    pub site_hash: u32,
    pub site_addr: u32,
    pub site_len: u32,
    /// The stock bytes at the site.
    pub stock: Vec<u8>,
    /// The offset of the trampoline in the image.
    pub tramp_off: u32,
    /// The displaced instructions, as plan lines (see [`prim::fmt_displaced`]); empty when the hook drops them.
    pub displaced: Vec<String>,
    /// Where the trampoline jumps back to.
    pub ret: u32,
}

fn put16(out: &mut Vec<u8>, w: u16) {
    out.extend_from_slice(&w.to_be_bytes());
}

/// The bit of a register in a `movem` mask: d0-d7 are bits 0-7, a0-a7 bits 8-15.
fn reg_bit(r: &str) -> Result<u32> {
    let n: u32 = r
        .get(1..)
        .and_then(|n| n.parse().ok())
        .filter(|n| *n <= 7)
        .ok_or_else(|| err!("bad register {r:?}"))?;
    match r.as_bytes()[0] {
        b'd' => Ok(n),
        b'a' => Ok(8 + n),
        _ => Err(err!("bad register {r:?}")),
    }
}

/// The machine code of `gas_hook` (`fwgen.rs`) without its last `jmp`: save the registers, push the arguments last first, `jsr` the handler
/// (a 32-bit absolute address, written as 0), pop the arguments, restore. `handler_reloc_at` gets the offset of the address word, in the
/// returned bytes. The caller stores the handler's image offset there and adds a base-relative relocation.
///
/// ```text
/// lea -N(sp),sp          4fef d16        N = 4 * saved registers (only with saved registers)
/// movem.l regs,(sp)      48d7 mask       bit n = d<n>, bit 8+n = a<n>
/// move.l Dn,-(sp)        2f00+n          move.l An,-(sp): 2f08+n
/// move.l d16(sp),-(sp)   2f2f d16        d16 = sp+N of the hook + N + 4 * (arguments pushed before)
/// jsr abs.l              4eb9 addr32
/// addq.l #P,sp           508f | (P&7)<<9 (P = 4 * arguments, up to 8); above 8: lea P(sp),sp = 4fef P
/// movem.l (sp),regs      4cd7 mask
/// lea N(sp),sp           4fef N
/// ```
pub fn encode_prologue(plan: &HandlerPlan, handler_reloc_at: &mut Vec<usize>) -> Result<Vec<u8>> {
    let mut o = Vec::new();
    let size = 4 * plan.save.len() as u32;
    let mut mask = 0u32;
    for r in &plan.save {
        mask |= 1 << reg_bit(r)?;
    }
    if !plan.save.is_empty() {
        put16(&mut o, 0x4fef);
        put16(&mut o, (-(size as i32)) as i16 as u16);
        put16(&mut o, 0x48d7);
        put16(&mut o, mask as u16);
    }
    for (k, a) in plan.args.iter().rev().enumerate() {
        match a {
            HandlerArg::Reg(r) => put16(&mut o, 0x2f00 | reg_bit(r)? as u16),
            HandlerArg::Stack(n) => {
                let d16 = n + size + 4 * k as u32;
                if d16 > i16::MAX as u32 {
                    return Err(err!("handler argument sp+{n}: offset {d16} is too far"));
                }
                put16(&mut o, 0x2f2f);
                put16(&mut o, d16 as u16);
            }
        }
    }
    put16(&mut o, 0x4eb9);
    handler_reloc_at.push(o.len());
    o.extend_from_slice(&[0; 4]);
    let pop = 4 * plan.args.len() as u32;
    if pop > 8 {
        put16(&mut o, 0x4fef);
        put16(&mut o, pop as u16);
    } else if pop > 0 {
        put16(&mut o, 0x508f | ((pop & 7) << 9) as u16);
    }
    if !plan.save.is_empty() {
        put16(&mut o, 0x4cd7);
        put16(&mut o, mask as u16);
        put16(&mut o, 0x4fef);
        put16(&mut o, size as u16);
    }
    Ok(o)
}

/// The trampoline of a hook: the prologue, then (mode `before`) the displaced instructions with `jmp.l ret` from [`cf_tail`], or (mode
/// `replace`) only `jmp.l ret`. Also the offsets of the handler address words. The displaced instructions are copied into the file, so they must
/// not depend on where they run: [`cf_tail`] must give the same bytes at two bases.
fn trampoline(
    cf: &CfSite,
    mode: &str,
    plan: &HandlerPlan,
    hook: &str,
) -> Result<(Vec<u8>, Vec<usize>)> {
    let ret = cf.addr + cf.len as u64;
    if ret > u32::MAX as u64 {
        return Err(err!("{hook}: return address {ret:#x} is not 32 bits"));
    }
    let mut at = Vec::new();
    let mut t = encode_prologue(plan, &mut at)?;
    match mode {
        "before" => {
            let a = cf_tail(cf, 0, ret).map_err(|e| err!("{hook}: {e}"))?;
            let b = cf_tail(cf, 0x1000, ret).map_err(|e| err!("{hook}: {e}"))?;
            if a != b {
                return Err(err!("PC-relative displaced instruction at {hook}"));
            }
            t.extend(a);
        }
        "replace" => {
            t.extend_from_slice(&CF_JMP_L.to_be_bytes());
            t.extend_from_slice(&(ret as u32).to_be_bytes());
        }
        m => {
            return Err(err!(
                "{hook}: hook mode {m:?} cannot be bound (only \"before\" and \"replace\")"
            ));
        }
    }
    Ok((t, at))
}

/// The declared hook name whose `name_hash` is `hash`.
fn find_hook<'a>(declared: impl Iterator<Item = &'a str>, hash: u32) -> Result<&'a str> {
    let mut declared = declared;
    declared
        .find(|n| name_hash(n) == hash)
        .ok_or_else(|| err!("no hook declared on this firmware has the name hash {hash:#010x}"))
}

/// Appends the trampoline of one hook to `image` (4-byte aligned) and the relocation of its handler call; `handler` is the handler's image offset.
fn bind_site(
    hook: &str,
    site: &HookSite,
    plan: &HandlerPlan,
    handler: u32,
    image: &mut Vec<u8>,
    relocs: &mut Vec<(u32, u32)>,
) -> Result<BoundHook> {
    if site.old.len() > BIND_STOCK_MAX {
        return Err(err!(
            "{hook}: the site has {} bytes, over the {BIND_STOCK_MAX} a BIND item holds",
            site.old.len()
        ));
    }
    let (tramp, at) = trampoline(&site.cf, &site.mode, plan, hook)?;
    let to32 = |v: u64, what: &str| {
        u32::try_from(v).map_err(|_| err!("{hook}: {what} {v:#x} is not 32 bits"))
    };
    while image.len() % 4 != 0 {
        image.push(0);
    }
    let tramp_off = image.len();
    image.extend_from_slice(&tramp);
    for o in at {
        let w = tramp_off + o;
        image[w..w + 4].copy_from_slice(&handler.to_be_bytes());
        relocs.push((w as u32, 0));
    }
    Ok(BoundHook {
        hook: hook.to_string(),
        site_hash: name_hash(hook),
        site_addr: to32(site.site, "site address")?,
        site_len: site.site_len as u32,
        stock: site.old.clone(),
        tramp_off: tramp_off as u32,
        displaced: if site.mode == "before" {
            prim::fmt_displaced(&site.cf)
        } else {
            vec![]
        },
        ret: to32(site.ret, "return address")?,
    })
}

/// The header table of `reg` with the `BIND` list `binds` (site hash, address, trampoline offset, stock bytes); every other list is kept.
fn table_with_bind(reg: &[u8], binds: &[(u32, u32, u32, Vec<u8>)]) -> Result<Vec<u8>> {
    let k = Keys::parse(reg).map_err(|e| err!("header table: {e:?}"))?;
    let needs: Vec<_> = k.needs().collect();
    let uses: Vec<_> = k.uses().collect();
    let kinds: Vec<_> = k.kinds().collect();
    let slots: Vec<_> = k.slots().collect();
    let hooks: Vec<_> = k.hooks().collect();
    let exports: Vec<_> = k.exports().collect();
    let resources: Vec<_> = k.resources().collect();
    let provides: Vec<Pair> = k.provides().map(|(a, b)| Pair { a, b }).collect();
    let groups: Vec<(u32, Vec<u32>)> = k
        .key_groups()
        .map(|(kind, ks)| (kind, ks.collect()))
        .collect();
    let key_groups: Vec<KeyGroup> = groups
        .iter()
        .map(|(kind, keys)| KeyGroup { kind: *kind, keys })
        .collect();
    let bind: Vec<Bind> = binds
        .iter()
        .map(|(site, addr, tramp, stock)| Bind {
            site: *site,
            addr: *addr,
            tramp: *tramp,
            stock,
        })
        .collect();
    let t = Table {
        needs: &needs,
        uses: &uses,
        keys: &key_groups,
        kinds: &kinds,
        slots: &slots,
        hooks: &hooks,
        exports: &exports,
        resources: &resources,
        provides: &provides,
        bind: &bind,
    };
    let mut out = vec![0u8; table_size(&t)];
    write_table(&t, &mut out).map_err(|e| err!("header table: {e:?}"))?;
    Ok(out)
}

/// The module's `.bss` starts at the end of the image, so the trampolines appended to the image (`image.len() - old` bytes) move it. Every
/// base-relative relocation whose stored word is an image offset at or past `old` addresses `.bss`: add the growth to it. A kind 1 word is
/// an addend to an import address, not an image offset: it stays. A word past `old + bss` is a malformed file. The entry points and the
/// `HOOKS` and `EXPORTS` offsets must be code offsets below `old`: they do not move.
fn shift_bss_words(image: &mut [u8], dkm: &Dkm, old: usize) -> Result<()> {
    let grow = image.len() - old;
    let end = old as u64 + dkm.bss as u64;
    for e in [dkm.init, dkm.fini, dkm.tick, dkm.menu]
        .into_iter()
        .flatten()
    {
        if e as usize >= old {
            return Err(err!("{}: entry point {e:#x} is not in the image", dkm.name));
        }
    }
    let keys = Keys::parse(&dkm.register).map_err(|e| err!("header table: {e:?}"))?;
    for p in keys.exports().chain(keys.hooks()) {
        if p.b as usize >= old {
            return Err(err!(
                "{}: table entry {:#x} is not in the image",
                dkm.name,
                p.b
            ));
        }
    }
    for (off, k) in &dkm.relocs {
        if k.is_some() {
            continue;
        }
        let at = *off as usize;
        let w = u32::from_be_bytes(image[at..at + 4].try_into().unwrap());
        if w as u64 > end {
            return Err(err!(
                "{}: relocated word at {at:#x} is {w:#x}, past the image and bss ({end:#x})",
                dkm.name
            ));
        }
        if w as usize >= old {
            image[at..at + 4].copy_from_slice(&(w + grow as u32).to_be_bytes());
        }
    }
    Ok(())
}

/// Binds the service `dks` to the stock firmware `fw`: the bound file and one report per hook. The file is refused when it is no service, declares no
/// hook or more than [`BIND_MAX`], is bound already, or a hook is not declared for this firmware or cannot be copied to another place.
pub fn bind(
    dks: &[u8],
    fw: &[u8],
    reg: &Registry,
    decls: &[&str],
) -> Result<(Vec<u8>, Vec<BoundHook>)> {
    let dkm = Dkm::parse(dks)?;
    if dkm.role != ROLE_SERVICE {
        return Err(err!("{}: not a service (role {})", dkm.name, dkm.role));
    }
    let keys = Keys::parse(&dkm.register).map_err(|e| err!("header table: {e:?}"))?;
    let hooks: Vec<Pair> = keys.hooks().collect();
    if hooks.is_empty() {
        return Err(err!("{}: declares no hooks (HOOKS is empty)", dkm.name));
    }
    if hooks.len() > BIND_MAX {
        return Err(err!(
            "{}: declares {} hooks, over the {BIND_MAX} a service binds",
            dkm.name,
            hooks.len()
        ));
    }
    if keys.bind_count() != 0 {
        return Err(err!("{}: is already bound (it has BIND entries)", dkm.name));
    }
    let f = Firmware::parse(fw)?;
    let port = reg.find(&f.syx_sha256).ok_or_else(|| {
        err!(
            "no port for firmware {}: it is not a stock image patchkit knows",
            f.syx_sha256
        )
    })?;
    let device = f.device.short();
    let mut image = dkm.image.clone();
    let old = image.len();
    let mut relocs: Vec<(u32, u32)> = dkm
        .relocs
        .iter()
        .map(|(off, k)| (*off, k.map_or(0, |i| 1 << 24 | i as u32)))
        .collect();
    let mut bound = Vec::new();
    for h in &hooks {
        let name = find_hook(port.hook.iter().map(|x| x.name.as_str()), h.a)?;
        if h.b as usize >= dkm.image.len() {
            return Err(err!(
                "{name}: handler offset {:#x} is outside the image ({} bytes)",
                h.b,
                dkm.image.len()
            ));
        }
        let site = resolve_hook_site(fw, reg, name)?;
        let plan = handler_plan(decls, device, name)?;
        bound.push(bind_site(name, &site, &plan, h.b, &mut image, &mut relocs)?);
    }
    shift_bss_words(&mut image, &dkm, old)?;
    relocs.sort();
    let items: Vec<(u32, u32, u32, Vec<u8>)> = bound
        .iter()
        .map(|b| (b.site_hash, b.site_addr, b.tramp_off, b.stock.clone()))
        .collect();
    let table = table_with_bind(&dkm.register, &items)?;
    let imports: Vec<&str> = dkm.imports.iter().map(String::as_str).collect();
    let file = emit(&EmitParts {
        name: &dkm.name,
        version: dkm.version,
        role: dkm.role,
        runtime_word: dkm.runtime_min | if dkm.trace { FLAG_TRACE } else { 0 },
        image: &image,
        bss: dkm.bss,
        align: dkm.align,
        imports: &imports,
        relocs: &relocs,
        entries: [dkm.init, dkm.fini, dkm.tick, dkm.menu],
        table: &table,
        dsp: &dkm.dsp,
        max_file: max_file(),
    })?;
    Ok((file, bound))
}

/// The report of [`bind`] as JSON text: one object per hook.
pub fn bind_json(hooks: &[BoundHook]) -> String {
    let list: Vec<_> = hooks
        .iter()
        .map(|h| {
            serde_json::json!({
                "hook": h.hook,
                "site_hash": format!("{:#010x}", h.site_hash),
                "site_addr": format!("{:#010x}", h.site_addr),
                "site_len": h.site_len,
                "stock": hex(&h.stock),
                "tramp_off": h.tramp_off,
                "displaced": h.displaced,
                "ret": format!("{:#010x}", h.ret),
            })
        })
        .collect();
    serde_json::to_string_pretty(&list).unwrap_or_default() + "\n"
}

/// `p` as an absolute path without `.` and `..` parts, made without touching the file system (a symbolic link is not followed).
fn lexical_abs(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut v = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::ParentDir => {
                v.pop();
            }
            std::path::Component::CurDir => {}
            c => v.push(c),
        }
    }
    v
}

/// Whether the bound file may be written to `out`: a bound file holds firmware bytes and must never be committed, so a path in the source tree
/// `root` is refused unless it is under `<root>/dist/` or `<root>/out/` (both ignored by git). A path outside the tree is allowed.
pub fn output_allowed(root: &Path, out: &Path) -> bool {
    let (root, out) = (lexical_abs(root), lexical_abs(out));
    !out.starts_with(&root)
        || out.starts_with(root.join("dist"))
        || out.starts_with(root.join("out"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dkm::{Options, Provided, pack_keys};

    fn plan(save: &[&'static str], args: &[&str]) -> HandlerPlan {
        HandlerPlan {
            save: save.to_vec(),
            args: args
                .iter()
                .map(|a| HandlerArg::Reg(a.to_string()))
                .collect(),
        }
    }

    // Layout of the synthetic firmware: moveq #1,d0 (2 bytes) ; move.l #0x12345678,d1 (6 bytes) ; rts. A 6-byte jump covers the first two
    // instructions: an 8-byte site.
    const CODE: [u8; 12] = [
        0x70, 0x01, 0x22, 0x3c, 0x12, 0x34, 0x56, 0x78, 0x4e, 0x75, 0x4e, 0x71,
    ];
    const BASE: u64 = 0x4000_0000;

    fn site_of(code: &[u8], mode: &str) -> Result<HookSite> {
        let cf = prim::cf_site(code, BASE, BASE, BASE)?;
        Ok(HookSite {
            port_id: "t-1".into(),
            anchor: "a".into(),
            mode: mode.into(),
            site: BASE,
            site_len: cf.len,
            old: code[..cf.len].to_vec(),
            ret: BASE + cf.len as u64,
            cf,
        })
    }

    #[test]
    fn prologue_for_one_stack_free_argument() {
        // cf.menu.hook.settings_list.tail: args [a2], clobbers d0-d1 and a0-a1, so nothing is saved.
        //   move.l a2,-(sp)   = 0x2f00 | (An: 8 + 2)        = 2f0a
        //   jsr abs.l         = 4eb9 + 32-bit address (0)   = 4eb9 00000000   (the address word is at offset 2 + 2 = 4)
        //   addq.l #4,sp      = 0x508f | (4 & 7) << 9       = 588f
        let mut at = vec![];
        let p = encode_prologue(&plan(&[], &["a2"]), &mut at).unwrap();
        assert_eq!(hex(&p), "2f0a4eb900000000588f");
        assert_eq!(at, [4]);
    }

    #[test]
    fn prologue_saves_and_restores_the_registers() {
        // save d0, d1, a0, a1 (16 bytes), argument a2:
        //   lea -16(sp),sp          = 4fef fff0
        //   movem.l d0-d1/a0-a1,(sp) = 48d7 mask; bits d0=0, d1=1, a0=8, a1=9 = 0x0303
        //   move.l a2,-(sp)         = 2f0a
        //   jsr abs.l               = 4eb9 00000000   (address word at offset 12)
        //   addq.l #4,sp            = 588f
        //   movem.l (sp),d0-d1/a0-a1 = 4cd7 0303
        //   lea 16(sp),sp           = 4fef 0010
        let mut at = vec![];
        let p = encode_prologue(&plan(&["d0", "d1", "a0", "a1"], &["a2"]), &mut at).unwrap();
        assert_eq!(
            hex(&p),
            "4feffff048d703032f0a4eb900000000588f4cd703034fef0010"
        );
        assert_eq!(at, [12]);
    }

    #[test]
    fn stacked_arguments_are_pushed_last_first() {
        // args sp+4, sp+8, nothing saved: k = 0 pushes sp+8 as 8(sp), k = 1 pushes sp+4 as 4+4 = 8(sp) again (the first push moved sp by 4).
        //   move.l 8(sp),-(sp) = 2f2f 0008 ; move.l 8(sp),-(sp) = 2f2f 0008 ; jsr ; addq.l #8,sp = 508f | 0 = 508f
        let p = HandlerPlan {
            save: vec![],
            args: vec![HandlerArg::Stack(4), HandlerArg::Stack(8)],
        };
        let mut at = vec![];
        let b = encode_prologue(&p, &mut at).unwrap();
        assert_eq!(hex(&b), "2f2f00082f2f00084eb900000000508f");
        assert_eq!(at, [10]);
    }

    #[test]
    fn a_site_binds_with_its_stock_bytes_and_a_handler_relocation() {
        let site = site_of(&CODE, "before").unwrap();
        assert_eq!(site.site_len, 8);
        let mut image = vec![0x70, 0x00, 0x4e, 0x75, 0x4e, 0x71];
        let mut relocs = vec![];
        let h = bind_site(
            "x.hook",
            &site,
            &plan(&[], &["a2"]),
            0x24,
            &mut image,
            &mut relocs,
        )
        .unwrap();
        // the stock bytes are the site's; the trampoline starts after the image padded to 4
        assert_eq!(h.stock, CODE[..8]);
        assert_eq!(
            (h.tramp_off, h.site_addr, h.site_len, h.ret),
            (8, BASE as u32, 8, 0x4000_0008)
        );
        assert_eq!(h.site_hash, name_hash("x.hook"));
        assert_eq!(h.displaced.len(), 2);
        // prologue, the displaced instructions as they were, jmp.l ret
        let mut want = vec![0x2f, 0x0a, 0x4e, 0xb9, 0, 0, 0, 0x24, 0x58, 0x8f];
        want.extend_from_slice(&CODE[..8]);
        want.extend_from_slice(&[0x4e, 0xf9, 0x40, 0x00, 0x00, 0x08]);
        assert_eq!(&image[..6], [0x70, 0x00, 0x4e, 0x75, 0x4e, 0x71]);
        assert_eq!(&image[6..8], [0, 0]);
        assert_eq!(&image[8..], want);
        // the handler word is relocated base-relative
        assert_eq!(relocs, [(12, 0)]);
    }

    #[test]
    fn a_site_over_the_stock_limit_is_refused() {
        let mut site = site_of(&CODE, "before").unwrap();
        site.old = vec![0; BIND_STOCK_MAX];
        let mut image = vec![0; 4];
        assert!(bind_site("x.hook", &site, &plan(&[], &[]), 0, &mut image, &mut vec![]).is_ok());
        site.old = vec![0; BIND_STOCK_MAX + 1];
        let e =
            bind_site("x.hook", &site, &plan(&[], &[]), 0, &mut image, &mut vec![]).unwrap_err();
        assert!(e.0.contains("over the 64"), "{e}");
    }

    fn synth_service(image: Vec<u8>, relocs: Vec<(u32, Option<usize>)>, bss: u32) -> Dkm {
        let t = Table::default();
        let mut table = vec![0u8; table_size(&t)];
        write_table(&t, &mut table).unwrap();
        Dkm {
            name: "svc".into(),
            version: 0x010000,
            runtime_min: 0,
            trace: false,
            role: ROLE_SERVICE,
            sha256: String::new(),
            image,
            bss,
            align: 4,
            relocs,
            imports: vec![],
            init: Some(0),
            fini: None,
            menu: None,
            tick: None,
            register: table,
            dsp: vec![],
        }
    }

    #[test]
    fn bss_references_move_with_the_appended_trampolines() {
        // image of 8 bytes: word at 0 addresses the image (2), word at 4 addresses bss + 0 (= old = 8)
        let mut image = vec![0, 0, 0, 2, 0, 0, 0, 8];
        let old = image.len();
        let dkm = synth_service(image.clone(), vec![(0, None), (4, None)], 16);
        let site = site_of(&CODE, "before").unwrap();
        let mut relocs = vec![];
        let h = bind_site(
            "x.hook",
            &site,
            &plan(&[], &["a2"]),
            0,
            &mut image,
            &mut relocs,
        )
        .unwrap();
        let grow = image.len() - old;
        assert_eq!(h.tramp_off as usize, old);
        assert_eq!(grow, 10 + 8 + 6);
        shift_bss_words(&mut image, &dkm, old).unwrap();
        let word = |at: usize| u32::from_be_bytes(image[at..at + 4].try_into().unwrap());
        assert_eq!(word(4), (old + grow) as u32);
        assert_eq!(word(0), 2);
        assert_eq!(image.len(), old + grow);
        assert_eq!(dkm.bss, 16);
        assert_eq!(&image[old..old + 2], [0x2f, 0x0a]);
    }

    #[test]
    fn a_word_past_image_and_bss_is_refused() {
        let mut image = vec![0, 0, 0, 0x19, 0, 0, 0, 0];
        let old = image.len();
        // old 8 + bss 16 = 24 (0x18): 0x19 is out of range
        let dkm = synth_service(image.clone(), vec![(0, None)], 16);
        image.extend_from_slice(&[0; 4]);
        let e = shift_bss_words(&mut image, &dkm, old).unwrap_err();
        assert!(e.0.contains("past the image and bss"), "{e}");
    }

    #[test]
    fn a_pc_relative_displaced_instruction_is_refused() {
        // bne.w (4 bytes) ; nop ; nop ; nop: the branch becomes the 32-bit form whose displacement depends on where it runs
        let code = [0x66, 0x00, 0x00, 0x20, 0x4e, 0x71, 0x4e, 0x71, 0x4e, 0x71];
        let site = site_of(&code, "before").unwrap();
        let mut image = vec![0; 4];
        let e =
            bind_site("x.hook", &site, &plan(&[], &[]), 0, &mut image, &mut vec![]).unwrap_err();
        assert!(
            e.0.contains("PC-relative displaced instruction at x.hook"),
            "{e}"
        );
        // an abs.l form of the same kind of operand is position independent: lea (0x10,pc),a0 ; nop
        let code = [0x41, 0xfa, 0x00, 0x10, 0x4e, 0x71, 0x4e, 0x71];
        let site = site_of(&code, "before").unwrap();
        assert!(bind_site("x.hook", &site, &plan(&[], &[]), 0, &mut image, &mut vec![]).is_ok());
    }

    #[test]
    fn an_unknown_hook_hash_is_refused() {
        let names = ["cf.menu.hook.a", "cf.menu.hook.b"];
        assert_eq!(
            find_hook(names.into_iter(), name_hash("cf.menu.hook.b")).unwrap(),
            "cf.menu.hook.b"
        );
        let e = find_hook(names.into_iter(), name_hash("cf.menu.hook.c")).unwrap_err();
        assert!(e.0.contains("no hook declared"), "{e}");
    }

    fn service_file(hooks: &[Pair], binds: &[Bind]) -> Vec<u8> {
        let t = Table {
            hooks,
            bind: binds,
            ..Table::default()
        };
        let mut table = vec![0u8; table_size(&t)];
        write_table(&t, &mut table).unwrap();
        let o = Options {
            name: "svc".into(),
            version: 0x010000,
            trace: false,
            stereo: false,
            gate: false,
            service: Some(Provided {
                service: "svc-test".into(),
                exports: vec![],
                hooks: vec![],
            }),
        };
        pack_keys(None, &table, &[], &o).unwrap()
    }

    #[test]
    fn a_bound_file_is_refused() {
        let stock = [0x4e, 0x71, 0x4e, 0x71, 0x4e, 0x71, 0x4e, 0x71];
        let h = name_hash("cf.menu.hook.settings_list.tail");
        let hooks = [Pair { a: h, b: 0 }];
        let binds = [Bind {
            site: h,
            addr: 0x4000_0000,
            tramp: 0,
            stock: &stock,
        }];
        let dks = service_file(&hooks, &binds);
        // refused before the firmware is looked at
        let e = bind(&dks, &[], &Registry::default(), &[]).unwrap_err();
        assert!(e.0.contains("already bound"), "{e}");
    }

    #[test]
    fn more_than_24_hooks_and_no_hooks_are_refused() {
        let hooks: Vec<Pair> = (0..BIND_MAX as u32 + 1).map(|a| Pair { a, b: 0 }).collect();
        let dks = service_file(&hooks, &[]);
        let e = bind(&dks, &[], &Registry::default(), &[]).unwrap_err();
        assert!(e.0.contains("over the 24"), "{e}");
        let dks = service_file(&hooks[..BIND_MAX], &[]);
        // 24 pass the count check and fail on the firmware
        let e = bind(&dks, &[], &Registry::default(), &[]).unwrap_err();
        assert!(!e.0.contains("over the 24"), "{e}");
        let dks = service_file(&[], &[]);
        let e = bind(&dks, &[], &Registry::default(), &[]).unwrap_err();
        assert!(e.0.contains("declares no hooks"), "{e}");
    }

    #[test]
    fn bind_items_round_trip_through_the_table() {
        let h = name_hash("cf.menu.hook.settings_list.tail");
        let hooks = [Pair { a: h, b: 0x20 }];
        let dks = service_file(&hooks, &[]);
        let dkm = Dkm::parse(&dks).unwrap();
        // a 6-byte stock is padded to 8 in the table; the item reports 6
        let stock = vec![1, 2, 3, 4, 5, 6];
        let items = [
            (h, 0x400a_5eac, 0x30, stock.clone()),
            (h ^ 1, 0x400a_6000, 0x58, vec![9; BIND_STOCK_MAX]),
        ];
        let t = table_with_bind(&dkm.register, &items).unwrap();
        let k = Keys::parse(&t).unwrap();
        assert_eq!(k.bind_count(), 2);
        let b = k.bind(0).unwrap();
        assert_eq!(
            (b.site, b.addr, b.len, b.tramp, b.stock),
            (h, 0x400a_5eac, 6, 0x30, &stock[..])
        );
        let b = k.bind(1).unwrap();
        assert_eq!(
            (b.site, b.addr, b.len, b.tramp, b.stock.len()),
            (h ^ 1, 0x400a_6000, 64, 0x58, 64)
        );
        assert!(k.bind(2).is_none());
        // the other lists stay
        assert!(k.hooks().eq(hooks.iter().copied()));
        assert!(k.provides().eq(Keys::parse(&dkm.register).unwrap().provides()));
    }

    #[test]
    fn the_output_is_refused_in_the_source_tree_outside_dist_and_out() {
        let root = Path::new("/src/digikit");
        assert!(output_allowed(root, Path::new("/src/digikit/dist/svc.dks")));
        assert!(output_allowed(root, Path::new("/src/digikit/out/a/svc.dks")));
        assert!(output_allowed(root, Path::new("/tmp/svc.dks")));
        assert!(!output_allowed(root, Path::new("/src/digikit/svc.dks")));
        assert!(!output_allowed(root, Path::new("/src/digikit/modules/svc.dks")));
        assert!(!output_allowed(root, Path::new("/src/digikit/distant/svc.dks")));
        // `..` does not get around the rule
        assert!(!output_allowed(
            root,
            Path::new("/src/digikit/dist/../modules/svc.dks")
        ));
        assert!(!output_allowed(root, Path::new("/src/digikit/dist/../../digikit/x")));
    }
}
