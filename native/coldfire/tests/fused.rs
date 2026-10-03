//! `Cpu::run_fused` against stepping: from the same machine (body already
//! decoded), k fused iterations and k x instructions single steps must give
//! the same `Cpu` (registers, lazy flags, counters, decode cache) and the
//! same memory, across random registers, overlaps, flag edge cases, plain-RAM
//! holes and writes into decoded code.

use coldfire::fused::Loop;
use coldfire::{Bus, BusError, Cpu};

const HEAD: u32 = 0x1000;
const SIZE: usize = 0x20000;

/// 128 KiB of RAM at 0. `hole` is RAM the bus reports as not plain.
#[derive(Clone)]
struct Ram {
    mem: Vec<u8>,
    hole: (u32, u32),
}

impl Ram {
    fn at(&self, a: u32, n: usize) -> Result<usize, BusError> {
        let i = a as usize;
        if i + n <= self.mem.len() {
            Ok(i)
        } else {
            Err(BusError {
                addr: a,
                write: false,
            })
        }
    }
}

impl Bus for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, BusError> {
        Ok(self.mem[self.at(a, 1)?])
    }
    fn read16(&mut self, a: u32) -> Result<u16, BusError> {
        let i = self.at(a, 2)?;
        Ok(u16::from_be_bytes([self.mem[i], self.mem[i + 1]]))
    }
    fn read32(&mut self, a: u32) -> Result<u32, BusError> {
        let i = self.at(a, 4)?;
        Ok(u32::from_be_bytes(self.mem[i..i + 4].try_into().unwrap()))
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        let i = self.at(a, 1)?;
        self.mem[i] = v;
        Ok(())
    }
    fn write16(&mut self, a: u32, v: u16) -> Result<(), BusError> {
        let i = self.at(a, 2)?;
        self.mem[i..i + 2].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
    fn write32(&mut self, a: u32, v: u32) -> Result<(), BusError> {
        let i = self.at(a, 4)?;
        self.mem[i..i + 4].copy_from_slice(&v.to_be_bytes());
        Ok(())
    }
    fn plain_ram(&self, addr: u32, len: u32) -> bool {
        let end = addr as u64 + len as u64;
        end <= self.mem.len() as u64 && (end <= self.hole.0 as u64 || addr >= self.hole.1)
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

fn machine(lp: Loop, rng: &mut Rng) -> (Cpu, Ram) {
    let mut mem = vec![0u8; SIZE];
    for b in mem.iter_mut() {
        *b = rng.next() as u8;
    }
    let code = lp.code();
    mem[HEAD as usize..HEAD as usize + code.len()].copy_from_slice(&code);
    // NOPs after the loop, where its exit branch lands
    for k in 0..16 {
        let at = HEAD as usize + code.len() + 2 * k;
        mem[at..at + 2].copy_from_slice(&0x4e71u16.to_be_bytes());
    }
    let mut cpu = Cpu::new();
    cpu.pc = HEAD;
    cpu.sr = 0x2700 | (rng.next() as u16 & 0x1f);
    for r in 0..8 {
        cpu.d[r] = rng.next();
        cpu.a[r] = rng.next();
    }
    cpu.a[7] = 0x1f000;
    (cpu, Ram { mem, hole: (0, 0) })
}

/// Fused vs stepped from `cpu`/`ram` (pc at the head, after one stepped
/// iteration so the body is decoded). Returns the fused iterations.
fn compare(lp: Loop, cpu: &Cpu, ram: &Ram, max: u32) -> u32 {
    // the runtime's precondition: stepping the body would only hit the
    // decode cache
    if lp.offsets().iter().any(|&o| !cpu.decoded_at(HEAD + o)) {
        return 0;
    }
    let (mut a, mut ram_a) = (cpu.clone(), ram.clone());
    let (mut b, mut ram_b) = (cpu.clone(), ram.clone());
    let k = a.run_fused(&mut ram_a, lp, max);
    assert!(k <= max);
    for _ in 0..k * lp.instructions() {
        b.step(&mut ram_b).unwrap();
        assert_eq!(b.last_exception, None);
    }
    assert_eq!(b.pc, HEAD);
    assert_eq!(a, b, "{lp:?} after {k} iterations");
    assert!(ram_a.mem == ram_b.mem, "{lp:?} memory after {k} iterations");
    // resolving the lazy flags must agree too
    a.resolve_nzv();
    b.resolve_nzv();
    assert_eq!(a.sr, b.sr);
    k
}

/// Step one iteration (decoding the body); false if the loop left or faulted.
fn warm(lp: Loop, cpu: &mut Cpu, ram: &mut Ram) -> bool {
    for _ in 0..lp.instructions() {
        if cpu.step(ram).is_err() || cpu.last_exception.is_some() {
            return false;
        }
    }
    cpu.pc == HEAD
}

fn fuzz(lp: Loop, seed: u64, setup: impl Fn(&mut Cpu, &mut Rng)) -> (u32, u32) {
    let mut rng = Rng(seed);
    let (mut runs, mut iterations) = (0, 0);
    for case in 0..400 {
        let (mut cpu, mut ram) = machine(lp, &mut rng);
        setup(&mut cpu, &mut rng);
        if case % 7 == 3 {
            let at = rng.below(SIZE as u32 - 0x100);
            ram.hole = (at, at + rng.below(0x100));
        }
        if !warm(lp, &mut cpu, &mut ram) {
            continue;
        }
        // arbitrary condition codes entering the batch (both sides)
        cpu.sr ^= rng.below(32) as u16;
        let max = [1, 2, 5, 40, 1000][case % 5];
        iterations += compare(lp, &cpu, &ram, max);
        runs += 1;
    }
    (runs, iterations)
}

fn addr(rng: &mut Rng) -> u32 {
    // mostly away from the code page, sometimes on it, sometimes misaligned
    match rng.below(8) {
        0 => HEAD - 0x40 + rng.below(0x80),
        1 => rng.below(SIZE as u32),
        _ => 0x4000 + 4 * rng.below(0x3000),
    }
}

#[test]
fn copy16_matches_stepping() {
    let (runs, its) = fuzz(Loop::Copy16, 1, |cpu, rng| {
        cpu.d[1] = [16, 16, 16, 4, 0xffff_fff0, rng.next()][rng.below(6) as usize];
        cpu.d[0] = match rng.below(4) {
            0 => rng.below(400),
            1 => 0x8000_0000u32.wrapping_add(rng.below(64)).wrapping_sub(32),
            2 => 0x7fff_fff0u32.wrapping_add(rng.below(32)),
            _ => rng.next(),
        };
        cpu.a[0] = addr(rng);
        cpu.a[1] = if rng.below(4) == 0 {
            cpu.a[0].wrapping_add(rng.below(32)).wrapping_sub(16)
        } else {
            addr(rng)
        };
    });
    assert!(runs > 100 && its > 1000, "{runs} {its}");
}

#[test]
fn clear16_matches_stepping() {
    let (runs, its) = fuzz(Loop::Clear16, 2, |cpu, rng| {
        cpu.d[1] = [16, 16, 4, 0xffff_fff0, rng.next()][rng.below(5) as usize];
        cpu.d[0] = match rng.below(3) {
            0 => rng.below(400),
            1 => 0x8000_0000u32.wrapping_add(rng.below(64)).wrapping_sub(32),
            _ => rng.next(),
        };
        cpu.a[0] = addr(rng);
    });
    assert!(runs > 100 && its > 1000, "{runs} {its}");
}

#[test]
fn interleave_matches_stepping() {
    for src in 1..=6u8 {
        let (runs, its) = fuzz(Loop::Interleave { src }, 3 + src as u64, |cpu, rng| {
            cpu.d[2] = match rng.below(3) {
                0 => rng.below(100),
                1 => 0xffff_fff0u32.wrapping_add(rng.below(32)),
                _ => 0x7fff_ffc0u32.wrapping_add(rng.below(128)),
            };
            cpu.d[0] = cpu.d[2].wrapping_add(rng.below(300)).wrapping_sub(20);
            cpu.a[0] = addr(rng);
            cpu.a[src as usize] = if rng.below(4) == 0 {
                cpu.a[0].wrapping_add(rng.below(16))
            } else {
                addr(rng)
            };
        });
        assert!(runs > 100 && its > 1000, "src {src}: {runs} {its}");
    }
}

/// Random EMAC state: the modes the firmware uses (MACSR 0x00, 0x20) and
/// the others (unsigned, rounding, saturation), accumulators, extensions,
/// PAV flags and MASK.
fn emac(cpu: &mut Cpu, rng: &mut Rng) {
    cpu.emac.macsr = [0x00, 0x20, 0x20, 0x00, 0x30, 0x40, 0x80, 0xa0, 0xc0][rng.below(9) as usize]
        | (rng.below(16) << 8) & 0xf00;
    for a in 0..4 {
        cpu.emac.acc[a] = rng.next();
    }
    cpu.emac.accext01 = rng.next();
    cpu.emac.accext23 = rng.next();
    cpu.emac.mask = if rng.below(2) == 0 {
        0xffff_ffff
    } else {
        0xffff_0000 | rng.below(0x10000)
    };
}

#[test]
fn mac_a_matches_stepping() {
    let (runs, its) = fuzz(Loop::MacA, 11, |cpu, rng| {
        emac(cpu, rng);
        cpu.d[3] = match rng.below(3) {
            0 => rng.below(200),
            1 => 0x8000_0000u32.wrapping_add(rng.below(64)),
            _ => rng.next(),
        };
        cpu.a[0] = addr(rng);
        cpu.a[1] = addr(rng);
        cpu.a[2] = if rng.below(4) == 0 {
            cpu.a[1].wrapping_add(rng.below(16))
        } else {
            addr(rng)
        };
    });
    assert!(runs > 100 && its > 1000, "{runs} {its}");
}

#[test]
fn mac_b_matches_stepping() {
    let (runs, its) = fuzz(Loop::MacB, 12, |cpu, rng| {
        emac(cpu, rng);
        cpu.d[0] = match rng.below(3) {
            0 => rng.below(200),
            1 => 0x8000_0000u32.wrapping_add(rng.below(64)),
            _ => rng.next(),
        };
        // (0,a0,d3.l*2) with d3 a sign-extended word stays in RAM
        cpu.a[0] = 0x10000 + rng.below(8) - 4;
        cpu.a[1] = addr(rng);
    });
    assert!(runs > 100 && its > 1000, "{runs} {its}");
}

#[test]
fn recognise_round_trips_and_rejects_neighbours() {
    for lp in [
        Loop::Copy16,
        Loop::Clear16,
        Loop::Interleave { src: 1 },
        Loop::Interleave { src: 6 },
        Loop::MacA,
        Loop::MacB,
    ] {
        let code = lp.code();
        assert_eq!(Loop::recognise(&code), Some(lp));
        for k in 0..code.len() {
            let mut broken = code.clone();
            broken[k] ^= 0x10;
            assert_ne!(Loop::recognise(&broken), Some(lp), "{lp:?} byte {k}");
        }
        assert_eq!(Loop::recognise(&code[..code.len() - 1]), None);
    }
    let mut a0_src = Loop::Interleave { src: 1 }.code();
    a0_src[13] &= !7;
    assert_eq!(Loop::recognise(&a0_src), None);
}

#[test]
#[ignore = "timing only"]
fn fused_speed() {
    for lp in [Loop::Interleave { src: 3 }, Loop::Copy16] {
        let mut rng = Rng(9);
        let (mut cpu, mut ram) = machine(lp, &mut rng);
        cpu.d[0] = 2_000_000;
        cpu.d[2] = 0;
        cpu.d[1] = 0;
        cpu.a[0] = 0x4000;
        cpu.a[1] = 0x8000;
        cpu.a[3] = 0x6000;
        assert!(warm(lp, &mut cpu, &mut ram));
        let iters = 200_000u32;
        let (mut a, mut ram_a) = (cpu.clone(), ram.clone());
        let t = std::time::Instant::now();
        let mut done = 0;
        while done < iters {
            // keep addresses in range: rewind like a new call would
            a.a[0] = 0x4000;
            a.a[1] = 0x8000;
            a.a[3] = 0x6000;
            done += a.run_fused(&mut ram_a, lp, 100);
        }
        let fused = t.elapsed();
        let (mut b, mut ram_b) = (cpu.clone(), ram.clone());
        let t = std::time::Instant::now();
        let mut k = 0;
        while k < iters {
            b.a[0] = 0x4000;
            b.a[1] = 0x8000;
            b.a[3] = 0x6000;
            for _ in 0..100 * lp.instructions() {
                b.step(&mut ram_b).unwrap();
            }
            k += 100;
        }
        let stepped = t.elapsed();
        eprintln!(
            "{lp:?}: fused {:?} stepped {:?} ({:.1}x)",
            fused,
            stepped,
            stepped.as_secs_f64() / fused.as_secs_f64()
        );
    }
}
