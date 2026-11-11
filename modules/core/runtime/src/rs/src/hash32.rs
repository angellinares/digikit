//! The +Drive content hash (`dksample::content_hash`: a lookup3 variant over big-endian words), streaming: the one file hash of the store
//! (DATA, END, VERIFY, the load checks). The table's 32-byte slot holds `hash | 1` as 4 big-endian bytes and 28 zeros (`slot`).
//!
//! The firmware variant keeps its last block of 1 to 12 bytes for the tail (`while n > 12`), so `update` mixes a block only when more than
//! 12 bytes are known. Up to 12 bytes are carried between updates. DATA chunks are multiples of 512 bytes, so the carry is a multiple of 4
//! and the words of a chunk stay 4-byte aligned (a long read on the big-endian ColdFire).

const SEED: u32 = 0x43fa_243a;

#[inline(always)]
fn rot(x: u32, n: u32) -> u32 {
    x.rotate_left(n)
}

/// One lookup3 mix of the block words `k0 k1 k2` (the same function as `dksample::hash`, written out as a chain).
#[inline(always)]
fn mix(a: &mut u32, b: &mut u32, c: &mut u32, k0: u32, k1: u32, k2: u32) {
    let c1 = c.wrapping_add(k2);
    let t2 = k1.wrapping_add(*b).wrapping_add(c1);
    let u1 = rot(c1, 4) ^ k0.wrapping_add(*a).wrapping_sub(c1);
    let t3 = t2.wrapping_add(u1);
    let u2 = k1.wrapping_add(*b).wrapping_sub(u1) ^ rot(u1, 6);
    let t4 = t3.wrapping_add(u2);
    let u3 = t2.wrapping_sub(u2) ^ rot(u2, 8);
    let t2b = t4.wrapping_add(u3);
    let u4 = t3.wrapping_sub(u3) ^ rot(u3, 16);
    let a2 = t2b.wrapping_add(u4);
    let u5 = t4.wrapping_sub(u4) ^ rot(u4, 19);
    *a = a2;
    *b = a2.wrapping_add(u5);
    *c = t2b.wrapping_sub(u5) ^ rot(u5, 4);
}

fn finalise(a: u32, b: u32, c: u32) -> u32 {
    let u1 = (b ^ c).wrapping_sub(rot(b, 14));
    let u2 = (u1 ^ a).wrapping_sub(rot(u1, 11));
    let u3 = (u2 ^ b).wrapping_sub(rot(u2, 25));
    let u4 = (u3 ^ u1).wrapping_sub(rot(u3, 16));
    let a2 = (u4 ^ u2).wrapping_sub(rot(u4, 4));
    let b2 = (a2 ^ u3).wrapping_sub(rot(a2, 14));
    (b2 ^ u4).wrapping_sub(rot(b2, 24))
}

#[inline(always)]
unsafe fn word(p: *const u8) -> u32 {
    if cfg!(target_endian = "big") && (p as usize) & 3 == 0 {
        *(p as *const u32)
    } else {
        ((*p as u32) << 24) | ((*p.add(1) as u32) << 16) | ((*p.add(2) as u32) << 8) | (*p.add(3) as u32)
    }
}

/// `n` (0 to 3) bytes at `p` as the high bytes of a big-endian word.
unsafe fn tail(p: *const u8, n: usize) -> u32 {
    let mut w = 0u32;
    let mut i = 0;
    while i < n {
        w |= (*p.add(i) as u32) << (24 - 8 * i as u32);
        i += 1;
    }
    w
}

#[repr(C)]
pub struct Hash32 {
    a: u32,
    b: u32,
    c: u32,
    /// Bytes carried (0 to 12; 0 only before the first byte).
    n: u32,
    buf: [u8; 12],
}

/// `dk_hash32` of <dk/platform/hash32.h> (the SDK's `hash32_*` calls work on a caller-owned state of this layout).
const _: () = assert!(core::mem::size_of::<Hash32>() == 28);

impl Hash32 {
    pub const fn new() -> Hash32 {
        Hash32 { a: SEED, b: SEED, c: SEED, n: 0, buf: [0; 12] }
    }

    #[inline(always)]
    unsafe fn block(&mut self, p: *const u8) {
        let (mut a, mut b, mut c) = (self.a, self.b, self.c);
        mix(&mut a, &mut b, &mut c, word(p), word(p.add(4)), word(p.add(8)));
        self.a = a;
        self.b = b;
        self.c = c;
    }

    /// Feed `n` bytes from `p`.
    pub unsafe fn update(&mut self, mut p: *const u8, mut n: u32) {
        if n == 0 {
            return;
        }
        let have = self.n;
        if have + n <= 12 {
            let mut i = 0;
            while i < n {
                self.buf[(have + i) as usize] = *p.add(i as usize);
                i += 1;
            }
            self.n = have + n;
            return;
        }
        // more than 12 bytes are known: the carried block (completed from `p`) is not the last one
        if have > 0 {
            let take = 12 - have;
            let mut i = 0;
            while i < take {
                self.buf[(have + i) as usize] = *p.add(i as usize);
                i += 1;
            }
            let b = self.buf.as_ptr();
            self.block(b);
            p = p.add(take as usize);
            n -= take;
        }
        while n > 12 {
            self.block(p);
            p = p.add(12);
            n -= 12;
        }
        // 1 to 12 bytes left: carried
        let mut i = 0;
        while i < n {
            self.buf[i as usize] = *p.add(i as usize);
            i += 1;
        }
        self.n = n;
    }

    pub fn finish(self) -> u32 {
        let (mut a, mut b, mut c) = (self.a, self.b, self.c);
        let n = self.n as usize;
        if n == 0 {
            return c;
        }
        let p = self.buf.as_ptr();
        unsafe {
            if n <= 4 {
                a = a.wrapping_add(tail(p, n));
            } else if n <= 8 {
                a = a.wrapping_add(word(p));
                b = b.wrapping_add(tail(p.add(4), n - 4));
            } else {
                a = a.wrapping_add(word(p));
                b = b.wrapping_add(word(p.add(4)));
                c = c.wrapping_add(tail(p.add(8), n - 8));
            }
        }
        finalise(a, b, c)
    }
}

/// The hash slot of the file hash `h`: `h | 1` (never 0: a folder's slot is all zero), big-endian, then 28 zeros.
pub fn slot(h: u32) -> [u8; 32] {
    let mut s = [0u8; 32];
    s[..4].copy_from_slice(&(h | 1).to_be_bytes());
    s
}

/// The hash slot of `n` bytes at `p`.
pub unsafe fn slot_of(p: *const u8, n: u32) -> [u8; 32] {
    let mut h = Hash32::new();
    h.update(p, n);
    slot(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slot_is_odd_big_endian_with_28_zeros() {
        let s = slot(0x1234_5678);
        assert_eq!(&s[..4], &[0x12, 0x34, 0x56, 0x79]);
        assert!(s[4..].iter().all(|&b| b == 0));
        assert_eq!(slot(0)[3], 1);
    }

    fn streamed(data: &[u8], cuts: &[usize]) -> u32 {
        let mut h = Hash32::new();
        let mut at = 0;
        for &c in cuts {
            let c = c.min(data.len());
            if c > at {
                unsafe { h.update(data.as_ptr().add(at), (c - at) as u32) };
                at = c;
            }
        }
        unsafe { h.update(data.as_ptr().add(at), (data.len() - at) as u32) };
        h.finish()
    }

    fn data(n: usize) -> Vec<u8> {
        let mut x = 0x1234_5678u32;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect()
    }

    /// `dksample::content_hash` of `data(n)` for each `n` (made with that function; the build copies this crate away from the repository, so it is not a dev-dependency).
    const VECTORS: &[(usize, u32)] = &[
        (0, 0x43fa243a),
        (1, 0x311f39cd),
        (2, 0x77595199),
        (3, 0x03d9d7d1),
        (4, 0x7f281151),
        (5, 0xa3e56e75),
        (6, 0x71796fa3),
        (7, 0x588949ce),
        (8, 0x3852e194),
        (9, 0x0a455d5c),
        (10, 0xb984e26f),
        (11, 0x7d07884b),
        (12, 0xe5b56d51),
        (13, 0x5a4ab629),
        (14, 0xd4279301),
        (15, 0x61d6284a),
        (16, 0x6588bbe8),
        (17, 0xb48a02fd),
        (18, 0x77959f01),
        (19, 0xc51ba03f),
        (20, 0xd1ffcd8c),
        (21, 0x2cc5399b),
        (22, 0x31b7e3f9),
        (23, 0xc18abefb),
        (24, 0x70004baf),
        (25, 0xe4ceea92),
        (26, 0x525999ff),
        (27, 0xae0a244d),
        (28, 0xcf7f782c),
        (29, 0x1539f78f),
        (30, 0x8405a788),
        (31, 0xc7411916),
        (32, 0x1e9bdbe9),
        (33, 0x88f160c0),
        (34, 0xdd6e0cd9),
        (35, 0xa6b8a886),
        (36, 0xe3505824),
        (37, 0xe340f9c3),
        (38, 0x044da797),
        (39, 0xd3ba48c2),
        (40, 0x591738c3),
        (41, 0xddd431d1),
        (42, 0x84098d5b),
        (43, 0x45906b48),
        (44, 0x991c8d3c),
        (45, 0x5f548cab),
        (46, 0xa31b74c6),
        (47, 0x86efe3c4),
        (48, 0xeeb5794e),
        (49, 0x74d6d286),
        (50, 0x7dfd63d1),
        (51, 0x04db010b),
        (52, 0xbfd0217e),
        (53, 0x1a10d7fb),
        (54, 0xede3843a),
        (55, 0xe1cf7b98),
        (56, 0x626d05c9),
        (57, 0x4e9878c2),
        (58, 0xa662a285),
        (59, 0xdcfdab54),
        (60, 0xfcb37467),
        (61, 0x4d471aca),
        (62, 0x562ae34d),
        (63, 0x9b743c63),
        (64, 0x41889f09),
        (511, 0x47f0a4ee),
        (512, 0xeda1aabd),
        (513, 0x07cd58c1),
        (4096, 0x9ceb7465),
        (32775, 0x1de5ed8a),
    ];

    #[test]
    fn whole_matches_dksample() {
        for &(n, want) in VECTORS {
            assert_eq!(streamed(&data(n), &[]), want, "len {n}");
        }
    }

    #[test]
    fn any_split_matches_dksample() {
        let mut x = 99u32;
        let mut rnd = |m: usize| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 8) as usize % m
        };
        for &(n, want) in VECTORS {
            let d = data(n);
            for _ in 0..40 {
                let k = rnd(8);
                let mut cuts: Vec<usize> = (0..k).map(|_| rnd(n + 1)).collect();
                cuts.sort();
                assert_eq!(streamed(&d, &cuts), want, "len {n} cuts {cuts:?}");
            }
            // chunks as DATA sends them: multiples of 512, the last one short
            let cuts: Vec<usize> = (1..).map(|i| i * 512).take_while(|&c| c < n).collect();
            assert_eq!(streamed(&d, &cuts), want, "len {n} 512-chunks");
        }
    }
}
