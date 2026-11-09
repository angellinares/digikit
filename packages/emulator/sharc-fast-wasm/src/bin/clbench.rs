//! `clbench DUMP_DIR [REPS_MS]`: replay the calls of a `SHARC_FAST_DUMP`
//! directory through the Cranelift backend (the gate's native column): check
//! each call against the recorded result, then time it. Same replay rules as
//! `gate.mjs`: context and register words are restored before every
//! repetition, window memory is not.

use sharc_native::fast::ir::*;
use std::collections::HashMap;
use std::time::Instant;

pub struct Call {
    pub kid: u64,
    pub insns: u64,
    pub code: u32,
    pub ctx_pre: Vec<u8>,
    pub ctx_post: Vec<u8>,
    pub st_off: u32,
    pub st_pre: Vec<u8>,
    pub st_post: Vec<u8>,
    /// (window slot, DM start, pre, post, host address)
    pub wins: Vec<(u32, u32, Vec<u8>, Vec<u8>, u64)>,
}

struct Rd<'a>(&'a [u8], usize);
impl Rd<'_> {
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes(self.0[self.1..self.1 + 4].try_into().unwrap());
        self.1 += 4;
        v
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        let v = self.0[self.1..self.1 + n].to_vec();
        self.1 += n;
        v
    }
}

pub fn parse_call(b: &[u8]) -> Call {
    let mut r = Rd(b, 0);
    assert_eq!(r.u32(), 0x3150_4143);
    let kid = r.u32() as u64 | (r.u32() as u64) << 32;
    let insns = r.u32() as u64 | (r.u32() as u64) << 32;
    let code = r.u32();
    let ctx_len = r.u32() as usize;
    let st_off = r.u32();
    let st_len = r.u32() as usize;
    let nwin = r.u32();
    let ctx_pre = r.bytes(ctx_len);
    let ctx_post = r.bytes(ctx_len);
    let st_pre = r.bytes(st_len);
    let st_post = r.bytes(st_len);
    let mut wins = Vec::new();
    for _ in 0..nwin {
        let (w, lo, len) = (r.u32(), r.u32(), r.u32() as usize);
        let host = r.u32() as u64 | (r.u32() as u64) << 32;
        let pre = r.bytes(len);
        let post = r.bytes(len);
        wins.push((w, lo, pre, post, host));
    }
    Call {
        kid,
        insns,
        code,
        ctx_pre,
        ctx_post,
        st_off,
        st_pre,
        st_post,
        wins,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let budget_ms: f64 = args.get(2).and_then(|x| x.parse().ok()).unwrap_or(40.0);
    let mut kernels: HashMap<u64, Kernel> = HashMap::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    for n in &files {
        if let Some(h) = n
            .strip_prefix("kernel-")
            .and_then(|x| x.strip_suffix(".bin"))
        {
            let b = std::fs::read(format!("{dir}/{n}")).unwrap();
            let w: Vec<u32> = b
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            kernels.insert(
                u64::from_str_radix(h, 16).unwrap(),
                Kernel::decode(&w).unwrap(),
            );
        }
    }
    let mut be = sharc_fast_cl::ClBackend::default();
    let (mut tot_ns, mut tot_insns, mut bad) = (0f64, 0u64, 0);
    let mut compile_ns = 0f64;
    println!("kid\tinsns\tns_call\tns_insn\tok");
    for n in &files {
        if !n.starts_with("call-") {
            continue;
        }
        let c = parse_call(&std::fs::read(format!("{dir}/{n}")).unwrap());
        let k = &kernels[&c.kid];
        let t0 = Instant::now();
        let ck = be.compile(k).unwrap();
        compile_ns += t0.elapsed().as_nanos() as f64;
        let mut ctx = vec![0u64; c.ctx_pre.len() / 8];
        let st_len = c.st_off as usize + c.st_pre.len();
        let mut st = vec![0u64; st_len / 8 + 2];
        // Windows that overlap in host memory share one arena (a read and a
        // write window of the same array alias).
        let mut order: Vec<usize> = (0..c.wins.len()).collect();
        order.sort_by_key(|&i| c.wins[i].4);
        let mut arenas: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut place = vec![(0usize, 0usize); c.wins.len()];
        for &i in &order {
            let (host, len) = (c.wins[i].4, c.wins[i].2.len());
            match arenas.last_mut() {
                Some((start, buf)) if host < *start + buf.len() as u64 => {
                    let end = (host - *start) as usize + len;
                    if end > buf.len() {
                        buf.resize(end, 0);
                    }
                }
                _ => arenas.push((host, vec![0u8; len])),
            }
            let ai = arenas.len() - 1;
            let off = (host - arenas[ai].0) as usize;
            arenas[ai].1[off..off + len].copy_from_slice(&c.wins[i].2);
            place[i] = (ai, off);
        }
        let wins = arenas; // restored never; the slots below point into it
        let setup = |ctx: &mut Vec<u64>, st: &mut Vec<u64>, wins: &Vec<(u64, Vec<u8>)>| {
            let cb = unsafe {
                std::slice::from_raw_parts_mut(ctx.as_mut_ptr().cast::<u8>(), c.ctx_pre.len())
            };
            cb.copy_from_slice(&c.ctx_pre);
            for (i, w) in c.wins.iter().enumerate() {
                let (ai, off) = place[i];
                let host = wins[ai].1.as_ptr() as u64 + off as u64;
                let slot = host.wrapping_sub(w.1 as u64);
                let at = CTX_WIN as usize + 8 * w.0 as usize;
                cb[at..at + 8].copy_from_slice(&slot.to_le_bytes());
            }
            let sb =
                unsafe { std::slice::from_raw_parts_mut(st.as_mut_ptr().cast::<u8>(), st_len) };
            sb[c.st_off as usize..].copy_from_slice(&c.st_pre);
        };
        setup(&mut ctx, &mut st, &wins);
        let code = unsafe { ck.run(ctx.as_mut_ptr().cast(), st.as_mut_ptr().cast()) };
        let cb = unsafe { std::slice::from_raw_parts(ctx.as_ptr().cast::<u8>(), c.ctx_pre.len()) };
        let sb = unsafe { std::slice::from_raw_parts(st.as_ptr().cast::<u8>(), st_len) };
        let win_lo = CTX_WIN as usize;
        let win_hi = win_lo + 8 * MAX_WIN as usize;
        let mut ok = code == c.code
            && cb[..win_lo] == c.ctx_post[..win_lo]
            && cb[win_hi..] == c.ctx_post[win_hi..]
            && sb[c.st_off as usize..] == c.st_post[..];
        for (i, w) in c.wins.iter().enumerate() {
            let (ai, off) = place[i];
            ok &= wins[ai].1[off..off + w.3.len()] == w.3[..];
        }
        if !ok {
            bad += 1;
            let diff = |a: &[u8], b: &[u8]| a.iter().zip(b).position(|(x, y)| x != y);
            if let Some(p) = diff(&sb[c.st_off as usize..], &c.st_post) {
                let at = p & !3;
                eprintln!(
                    "  st+{at}: pre {:02x?} want {:02x?} got {:02x?} (st_off {:#x}, k insts {})",
                    &c.st_pre[at..at + 4],
                    &c.st_post[at..at + 4],
                    &sb[c.st_off as usize + at..c.st_off as usize + at + 4],
                    c.st_off,
                    k.inst_count()
                );
            }
            eprintln!(
                "mismatch {n}: code {code} want {}; ctx_lo {:?} ctx_hi {:?} st {:?} wins {:?}",
                c.code,
                diff(&cb[..win_lo], &c.ctx_post[..win_lo]),
                diff(&cb[win_hi..], &c.ctx_post[win_hi..]).map(|x| x + win_hi),
                diff(&sb[c.st_off as usize..], &c.st_post),
                c.wins
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        let (ai, off) = place[i];
                        diff(&wins[ai].1[off..off + w.3.len()], &w.3)
                    })
                    .collect::<Vec<_>>()
            );
        }
        // Timing: restore the context and register words, not the windows.
        let mut reps = 0u64;
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f64() * 1e3 < budget_ms {
            for _ in 0..16 {
                setup(&mut ctx, &mut st, &wins);
                unsafe { ck.run(ctx.as_mut_ptr().cast(), st.as_mut_ptr().cast()) };
                reps += 1;
            }
        }
        let total = t0.elapsed().as_nanos() as f64;
        let t1 = Instant::now();
        for _ in 0..reps {
            setup(&mut ctx, &mut st, &wins);
        }
        let ns = (total - t1.elapsed().as_nanos() as f64) / reps as f64;
        println!(
            "{:016x}\t{}\t{:.0}\t{:.3}\t{}",
            c.kid,
            c.insns,
            ns,
            ns / c.insns as f64,
            ok
        );
        tot_ns += ns;
        tot_insns += c.insns;
    }
    println!(
        "TOTAL native cranelift: {} insns, {:.3} ns/insn (call + setup), {} mismatches, compile {:.0} us/kernel",
        tot_insns,
        tot_ns / tot_insns as f64,
        bad,
        compile_ns
            / 1e3
            / files
                .iter()
                .filter(|n| n.starts_with("call-"))
                .count()
                .max(1) as f64
    );
}
