//! SHA-256 (FIPS 180-4), streaming, 32-bit lengths only (a store file is at most 4 GiB - 1).

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

// The four sigma functions of FIPS 180-4 written as `(x ^ x >> a ^ x >> b) >> c ^ (x ^ x << d ^ x << e) << f`: the XOR of three rotations is the
// XOR of three right shifts and three left shifts, and each triple is one shifted value of a two-step sum (the rotations of a word do not
// overlap, so the OR of a rotation is the XOR). A ColdFire has no rotate: a rotation is two shifts, a move and an OR, and the factored form
// takes about a quarter fewer instructions.
/// Σ1(e) = rotr 6 ^ rotr 11 ^ rotr 25.
#[inline(always)]
fn big1(e: u32) -> u32 {
    ((e ^ (e >> 5) ^ (e >> 19)) >> 6) ^ ((e ^ (e << 14) ^ (e << 19)) << 7)
}
/// Σ0(a) = rotr 2 ^ rotr 13 ^ rotr 22.
#[inline(always)]
fn big0(a: u32) -> u32 {
    ((a ^ (a >> 11) ^ (a >> 20)) >> 2) ^ ((a ^ (a << 9) ^ (a << 20)) << 10)
}
/// σ0(x) = rotr 7 ^ rotr 18 ^ shr 3.
#[inline(always)]
fn small0(x: u32) -> u32 {
    ((x ^ (x >> 4) ^ (x >> 15)) >> 3) ^ ((x ^ (x << 11)) << 14)
}
/// σ1(y) = rotr 17 ^ rotr 19 ^ shr 10.
#[inline(always)]
fn small1(y: u32) -> u32 {
    ((y ^ (y >> 7) ^ (y >> 9)) >> 10) ^ ((y ^ (y << 2)) << 13)
}

/// `repr(C)`: `h` first and 4-byte aligned, so `buf` (offset 32) is too, and `block` reads its words whole.
#[repr(C)]
pub struct Sha {
    h: [u32; 8],
    buf: [u8; 64],
    n: u32,
    total: u32,
}

impl Sha {
    pub const fn new() -> Sha {
        Sha {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0; 64],
            n: 0,
            total: 0,
        }
    }

    /// One 64-byte block. Unrolled by hand (the rounds rename the eight variables instead of moving them, the message schedule is a ring of 16
    /// words in place, no index is checked): a ColdFire has no rotate and few registers, and a loop over `[u32; 64]` took about 1000 instructions
    /// a byte, which for a sample of several MiB was minutes of the MIDI task.
    unsafe fn block(&mut self, p: *const u8) {
        let mut w = [0u32; 16];
        let mut i = 0;
        if cfg!(target_endian = "big") && (p as usize) & 3 == 0 {
            // the ColdFire is big-endian: a long read is the message word (4 instructions a word less than four byte reads)
            let q = p as *const u32;
            while i < 16 {
                w[i] = *q.add(i);
                i += 1;
            }
        } else {
            while i < 16 {
                let q = p.add(i * 4);
                w[i] = ((*q as u32) << 24)
                    | ((*q.add(1) as u32) << 16)
                    | ((*q.add(2) as u32) << 8)
                    | (*q.add(3) as u32);
                i += 1;
            }
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        let k = K.as_ptr();
        // one round: the new `d` and `h`; the caller passes the variables in the rotated order
        macro_rules! rnd {
            ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $kw:expr, $w:expr) => {{
                let t1 = $h
                    .wrapping_add(big1($e))
                    .wrapping_add(($e & $f) ^ (!$e & $g))
                    .wrapping_add(*k.add($kw))
                    .wrapping_add($w);
                let t2 = big0($a).wrapping_add(($a & $b) ^ ($a & $c) ^ ($b & $c));
                $d = $d.wrapping_add(t1);
                $h = t1.wrapping_add(t2);
            }};
        }
        // the schedule word j (0..16) of this group of 16: w[j] += s0(w[j+1]) + w[j+9] + s1(w[j+14]) (indices mod 16)
        macro_rules! sched {
            ($j:expr) => {{
                let x = w[($j + 1) & 15];
                let y = w[($j + 14) & 15];
                w[$j] = w[$j]
                    .wrapping_add(small0(x))
                    .wrapping_add(w[($j + 9) & 15])
                    .wrapping_add(small1(y));
            }};
        }
        // 16 rounds from round `$r`; `$s` is 1 when the schedule words are made first
        macro_rules! sixteen {
            ($r:expr, $s:expr) => {{
                macro_rules! step {
                    ($j:expr, $a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident) => {{
                        if $s == 1 {
                            sched!($j);
                        }
                        rnd!($a, $b, $c, $d, $e, $f, $g, $h, $r + $j, w[$j]);
                    }};
                }
                step!(0, a, b, c, d, e, f, g, h);
                step!(1, h, a, b, c, d, e, f, g);
                step!(2, g, h, a, b, c, d, e, f);
                step!(3, f, g, h, a, b, c, d, e);
                step!(4, e, f, g, h, a, b, c, d);
                step!(5, d, e, f, g, h, a, b, c);
                step!(6, c, d, e, f, g, h, a, b);
                step!(7, b, c, d, e, f, g, h, a);
                step!(8, a, b, c, d, e, f, g, h);
                step!(9, h, a, b, c, d, e, f, g);
                step!(10, g, h, a, b, c, d, e, f);
                step!(11, f, g, h, a, b, c, d, e);
                step!(12, e, f, g, h, a, b, c, d);
                step!(13, d, e, f, g, h, a, b, c);
                step!(14, c, d, e, f, g, h, a, b);
                step!(15, b, c, d, e, f, g, h, a);
            }};
        }
        sixteen!(0, 0);
        let mut r = 16usize;
        while r < 64 {
            sixteen!(r, 1);
            r += 16;
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(h);
    }

    /// Feed `n` bytes from `p`.
    pub unsafe fn update(&mut self, mut p: *const u8, mut n: u32) {
        self.total = self.total.wrapping_add(n);
        while n > 0 {
            if self.n == 0 && n >= 64 {
                self.block(p);
                p = p.add(64);
                n -= 64;
                continue;
            }
            self.buf[self.n as usize] = *p;
            self.n += 1;
            p = p.add(1);
            n -= 1;
            if self.n == 64 {
                let b = self.buf.as_ptr();
                self.block(b);
                self.n = 0;
            }
        }
    }

    /// Feed `n` zero bytes.
    pub unsafe fn update_zeros(&mut self, n: u32) {
        let z = [0u8; 32];
        let mut left = n;
        while left > 0 {
            let k = if left > 32 { 32 } else { left };
            self.update(z.as_ptr(), k);
            left -= k;
        }
    }

    pub unsafe fn finish(mut self, out: *mut u8) {
        let bits_hi = self.total >> 29;
        let bits_lo = self.total << 3;
        let pad = [0x80u8];
        let zero = [0u8];
        self.update(pad.as_ptr(), 1);
        while self.n != 56 {
            self.update(zero.as_ptr(), 1);
        }
        let len = [
            (bits_hi >> 24) as u8,
            (bits_hi >> 16) as u8,
            (bits_hi >> 8) as u8,
            bits_hi as u8,
            (bits_lo >> 24) as u8,
            (bits_lo >> 16) as u8,
            (bits_lo >> 8) as u8,
            bits_lo as u8,
        ];
        self.update(len.as_ptr(), 8);
        let mut i = 0;
        while i < 8 {
            let x = self.h[i];
            *out.add(i * 4) = (x >> 24) as u8;
            *out.add(i * 4 + 1) = (x >> 16) as u8;
            *out.add(i * 4 + 2) = (x >> 8) as u8;
            *out.add(i * 4 + 3) = x as u8;
            i += 1;
        }
    }
}

/// SHA-256 of `n` bytes at `p` into `out[32]`.
pub unsafe fn sha256(p: *const u8, n: u32, out: *mut u8) {
    let mut s = Sha::new();
    s.update(p, n);
    s.finish(out);
}
