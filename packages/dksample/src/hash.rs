//! The +Drive content hash: the firmware's lookup3 variant (big-endian words, no length seed, an empty tail is not finalised).
//! The firmware stores `content_hash(file) | 1` in a file's record (word +0x0c) and in its hash table (finding 14).

fn rot(x: u32, n: u32) -> u32 {
    x.rotate_left(n)
}
fn mix(mut a: u32, mut b: u32, mut c: u32, k0: u32, k1: u32, k2: u32) -> (u32, u32, u32) {
    c = c.wrapping_add(k2);
    let t2 = k1.wrapping_add(b).wrapping_add(c);
    let u1 = rot(c, 4) ^ (k0.wrapping_add(a).wrapping_sub(c));
    let t3 = t2.wrapping_add(u1);
    let u2 = k1.wrapping_add(b).wrapping_sub(u1) ^ rot(u1, 6);
    let t4 = t3.wrapping_add(u2);
    let u3 = t2.wrapping_sub(u2) ^ rot(u2, 8);
    let t2b = t4.wrapping_add(u3);
    let u4 = t3.wrapping_sub(u3) ^ rot(u3, 16);
    a = t2b.wrapping_add(u4);
    let u5 = t4.wrapping_sub(u4) ^ rot(u4, 19);
    b = a.wrapping_add(u5);
    c = t2b.wrapping_sub(u5) ^ rot(u5, 4);
    (a, b, c)
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
fn tail(data: &[u8]) -> u32 {
    let mut b = [0; 4];
    b[..data.len()].copy_from_slice(data);
    u32::from_be_bytes(b)
}
/// The firmware's lookup3 variant with the seed `initval`.
pub fn hashlittle(data: &[u8], initval: u32) -> u32 {
    let mut a = initval.wrapping_add(0xdeadbeef);
    let mut b = a;
    let mut c = a;
    if data.is_empty() {
        return c;
    }
    let mut off = 0;
    let mut n = data.len();
    while n > 12 {
        let k0 = u32::from_be_bytes(data[off..off + 4].try_into().unwrap());
        let k1 = u32::from_be_bytes(data[off + 4..off + 8].try_into().unwrap());
        let k2 = u32::from_be_bytes(data[off + 8..off + 12].try_into().unwrap());
        (a, b, c) = mix(a, b, c, k0, k1, k2);
        off += 12;
        n -= 12;
    }
    if n <= 4 {
        a = a.wrapping_add(tail(&data[off..]));
    } else if n <= 8 {
        a = a.wrapping_add(u32::from_be_bytes(data[off..off + 4].try_into().unwrap()));
        b = b.wrapping_add(tail(&data[off + 4..]));
    } else {
        a = a.wrapping_add(u32::from_be_bytes(data[off..off + 4].try_into().unwrap()));
        b = b.wrapping_add(u32::from_be_bytes(
            data[off + 4..off + 8].try_into().unwrap(),
        ));
        c = c.wrapping_add(tail(&data[off + 8..]));
    }
    finalise(a, b, c)
}
pub fn content_hash(data: &[u8]) -> u32 {
    hashlittle(data, 0x43fa243a_u32.wrapping_sub(0xdeadbeef))
}
/// The digikit store's hash slot of a file (table entry, BEGIN, LIST, VERIFY): `content_hash | 1` (never 0: a folder's slot is all zero) as 4 big-endian
/// bytes, then 28 zeros. The runtime computes the same (`modules/core/runtime/src/rs/src/hash32.rs`).
pub fn file_hash(data: &[u8]) -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[..4].copy_from_slice(&(content_hash(data) | 1).to_be_bytes());
    slot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_file_hashes_to_the_seed() {
        assert_eq!(content_hash(b""), 0x43fa_243a);
    }

    #[test]
    fn the_store_slot_is_the_odd_hash_then_28_zeros() {
        let slot = file_hash(b"");
        assert_eq!(&slot[..4], &[0x43, 0xfa, 0x24, 0x3b]);
        assert!(slot[4..].iter().all(|&b| b == 0));
        assert_eq!(&file_hash(b"abc")[..4], &[0x49, 0xbe, 0xc8, 0x45]);
    }

    #[test]
    fn a_vector_of_the_firmware() {
        // lookup3 vectors with the superblock seed (the same ones plusdrive-format checks against the card)
        assert_eq!(hashlittle(&[0xe1], 0x3132_3334), 0x7eee_66cf);
        assert_eq!(hashlittle(&[], 0x3132_3334), 0x0fdf_f223);
    }
}
