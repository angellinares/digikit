use core::cmp::Ordering;

pub const SECTOR: usize = 512;
pub const PAGE: usize = 0x8000;
pub const RECORD_SIZE: usize = 0x80;
pub const RECORDS_PER_PAGE: usize = PAGE / RECORD_SIZE;
pub const ATTR_FILE: u8 = 0;
pub const ATTR_DIR: u8 = 1;
pub const DEFAULT_OFFSET01: u8 = 2;
pub const ROOT_ID: u32 = 2;
pub const ROOT_PARENT: u32 = ROOT_ID;
pub const HEADER_SECTOR: u64 = 0;
pub const POOL_TABLE_SECTOR: u64 = 0x800;
pub const BOOT_CONFIG_SECTOR: u64 = 0x40000;
pub const BOOT_CONFIG_SECTOR_2: u64 = 0x48000;
pub const FACTORY_TABLE_SECTOR: u64 = 0x458000;
pub const SUPERBLOCK_SECTOR: u32 = 0x5d8000;
pub const ID_BITMAP_SECTOR: u64 = 0x5d8040;
pub const PAGE_BITMAP_SECTOR: u64 = 0x5d80c0;
pub const RECORD_AREA_SECTOR: u64 = 0x5d8180;
pub const CONTENT_AREA_SECTOR: u64 = 0x5ee180;
pub const HASH_TABLE_SECTOR: u64 = 0x5ee980;
pub const RESERVED_PAGES: u32 = 0x78;
pub const DEFAULT_CAPACITY_BLOCKS: u32 = 0x0076_0000;
pub const NATIVE_HEADER_SIZE: usize = 0x40;
pub const NATIVE_TRAILER_SIZE: usize = 0x10;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FormatError {
    NameTooLong,
    DirectoryTooLarge,
    TooManyEntries,
    InvalidPcmLength,
    InvalidProjectHeaderSize,
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirEntry {
    pub id: u32,
    pub name: Vec<u8>,
    pub kind: u8,
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectoryPages {
    pub contents: Vec<u8>,
    pub by_hash: Vec<u8>,
    pub by_id: Vec<u8>,
    pub listing: Vec<u8>,
}

fn put16(out: &mut [u8], at: usize, v: u16) {
    out[at..at + 2].copy_from_slice(&v.to_be_bytes());
}
fn put32(out: &mut [u8], at: usize, v: u32) {
    out[at..at + 4].copy_from_slice(&v.to_be_bytes());
}
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
/// Firmware lookup3 variant: big-endian words, no length seed, empty tail unfinalized.
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
pub fn name_hash(name: &[u8]) -> u32 {
    let (mut cur, mut prev) = (0x12a3fe2d_u32, 0x37abe8f9_u32);
    for &byte in name {
        let mut v = (u32::from(byte).wrapping_mul(0x6d22f5) ^ cur).wrapping_add(prev);
        if v & 0x80000000 != 0 {
            v = v.wrapping_add(0x80000001)
        };
        prev = cur;
        cur = v;
    }
    cur.wrapping_mul(2)
}
pub fn build_superblock() -> [u8; SECTOR] {
    let mut b = [0; SECTOR];
    for (at, v) in [
        (0, 0x656b4653),
        (4, 4),
        (8, PAGE as u32),
        (12, 0x58000),
        (16, 0xa0080),
        (20, 0x40),
        (24, 0xc0),
        (28, 0x180),
        (32, 0x16180),
        (36, 0x40),
        (40, 0x20),
        (44, 0x5ee980),
        (48, 0x5ef480),
    ] {
        put32(&mut b, at, v)
    }
    let checksum = hashlittle(&b[..0x1fc], 0x31323334);
    put32(&mut b, 0x1fc, checksum);
    b
}
pub fn build_project_header(
    size: usize,
    seq: u32,
    fs_version: u32,
) -> Result<Vec<u8>, FormatError> {
    if size < 0x1c || (0x105..0x108).contains(&size) {
        return Err(FormatError::InvalidProjectHeaderSize);
    }
    let mut b = vec![0; size];
    put32(&mut b, 0, 0x434f4b69);
    put32(&mut b, 4, 3);
    if size >= 0x108 {
        put32(&mut b, 0x104, seq)
    }
    put32(&mut b, 0x18, fs_version);
    Ok(b)
}
pub fn build_boot_config_record() -> Vec<u8> {
    build_project_header(256, 0, 0).unwrap()
}
pub fn crc32_ieee_raw(seed: u32, data: &[u8]) -> u32 {
    let mut c = seed;
    for &x in data {
        c ^= u32::from(x);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb88320
            } else {
                c >> 1
            }
        }
    }
    c
}
pub fn build_factory_table_record() -> [u8; PAGE] {
    let mut b = [0; PAGE];
    put32(&mut b, 0, 0x4d61476a);
    b[4..8].copy_from_slice(&[0xa7, 0x27, 0x55, 0x8c]);
    put16(&mut b, 8, 0x2b);
    put16(&mut b, 10, 3);
    put32(&mut b, 12, 12);
    debug_assert_eq!(
        crc32_ieee_raw(crc32_ieee_raw(0xffffffff, &b[8..12]), &b[4..8]),
        0xdebb20e3
    );
    b
}
fn nat(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    loop {
        if i == a.len() {
            return if j < b.len() {
                Ordering::Less
            } else {
                Ordering::Equal
            };
        }
        if j == b.len() {
            return Ordering::Greater;
        }
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (mut i2, mut j2) = (i, j);
            while i2 < a.len() && a[i2].is_ascii_digit() {
                i2 += 1
            }
            while j2 < b.len() && b[j2].is_ascii_digit() {
                j2 += 1
            }
            let mut sa = &a[i..i2];
            let mut sb = &b[j..j2];
            while sa.len() > 1 && sa[0] == b'0' {
                sa = &sa[1..];
            }
            while sb.len() > 1 && sb[0] == b'0' {
                sb = &sb[1..];
            }
            if sa.len() != sb.len() {
                return sa.len().cmp(&sb.len());
            }
            if sa != sb {
                return sa.cmp(sb);
            }
            i = i2;
            j = j2;
            continue;
        }
        let ca = a[i].to_ascii_lowercase();
        let cb = b[j].to_ascii_lowercase();
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
        j += 1
    }
}
fn index(items: &[(u32, usize)]) -> Result<Vec<u8>, FormatError> {
    if items.len() > u16::MAX as usize {
        return Err(FormatError::TooManyEntries);
    }
    let mut out = vec![0; 8 + items.len() * 8];
    put16(&mut out, 0, items.len() as u16);
    for (i, (k, p)) in items.iter().enumerate() {
        put32(&mut out, 8 + i * 8, *k);
        put32(&mut out, 12 + i * 8, *p as u32)
    }
    Ok(out)
}
pub fn build_directory(
    dir_id: u32,
    parent_id: u32,
    children: &[DirEntry],
) -> Result<DirectoryPages, FormatError> {
    if children.iter().any(|e| e.name.len() > 255) {
        return Err(FormatError::NameTooLong);
    }
    let mut entries = Vec::with_capacity(children.len() + 2);
    entries.push(DirEntry {
        id: dir_id,
        name: b".".to_vec(),
        kind: ATTR_DIR,
    });
    entries.push(DirEntry {
        id: parent_id,
        name: b"..".to_vec(),
        kind: ATTR_DIR,
    });
    entries.extend_from_slice(children);
    if entries.len() > u16::MAX as usize {
        return Err(FormatError::TooManyEntries);
    }
    let mut contents = vec![0; PAGE];
    let mut pos = Vec::new();
    let mut at = 0;
    for (n, e) in entries.iter().enumerate() {
        let used = (e.name.len() + 11) & !3;
        if at + used > PAGE {
            return Err(FormatError::DirectoryTooLarge);
        }
        let slot = if n + 1 == entries.len() {
            PAGE - at
        } else {
            used
        };
        put32(&mut contents, at, e.id);
        put16(&mut contents, at + 4, slot as u16);
        contents[at + 6] = e.name.len() as u8;
        contents[at + 7] = e.kind;
        contents[at + 8..at + 8 + e.name.len()].copy_from_slice(&e.name);
        pos.push(at);
        at += used;
    }
    let mut by_hash: Vec<_> = entries
        .iter()
        .zip(&pos)
        .map(|(e, &p)| (name_hash(&e.name), p))
        .collect();
    by_hash.sort_by_key(|x| x.0);
    let mut ids: Vec<_> = entries.iter().zip(&pos).map(|(e, &p)| (e.id, p)).collect();
    ids.sort_by_key(|x| x.0);
    let mut order: Vec<usize> = (2..entries.len()).collect();
    order.sort_by(|&x, &y| {
        let dx = entries[x].kind == ATTR_DIR;
        let dy = entries[y].kind == ATTR_DIR;
        dy.cmp(&dx)
            .then_with(|| nat(&entries[x].name, &entries[y].name))
    });
    let mut listing = vec![
        (u32::from_be_bytes([b'.', 0, 0, 0]), pos[0]),
        (u32::from_be_bytes([b'.', b'.', 0, 0]), pos[1]),
    ];
    listing.extend(order.into_iter().map(|i| {
        let mut n = [0; 4];
        n[..entries[i].name.len().min(4)]
            .copy_from_slice(&entries[i].name[..entries[i].name.len().min(4)]);
        (u32::from_be_bytes(n), pos[i])
    }));
    Ok(DirectoryPages {
        contents,
        by_hash: index(&by_hash)?,
        listing: index(&listing)?,
        by_id: index(&ids)?,
    })
}
pub fn build_native_sample(pcm_be16: &[u8], stereo: bool) -> Result<Vec<u8>, FormatError> {
    if pcm_be16.len() % if stereo { 4 } else { 2 } != 0 {
        return Err(FormatError::InvalidPcmLength);
    }
    let len = u32::try_from(pcm_be16.len()).map_err(|_| FormatError::InvalidPcmLength)?;
    let mut out = vec![0; NATIVE_HEADER_SIZE];
    out[1] = u8::from(stereo);
    put32(&mut out, 4, len);
    put32(&mut out, 8, 48000);
    out[0x14] = 0x7f;
    out.extend_from_slice(pcm_be16);
    out.resize(out.len() + NATIVE_TRAILER_SIZE, 0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_empty_and_vectors() {
        assert_eq!(hashlittle(&[], 0x31323334), 0x0fdf_f223);
        assert_eq!(hashlittle(&[0xe1], 0x31323334), 0x7eee_66cf);
        assert_eq!(
            hashlittle(
                &[
                    0xed, 0x15, 0xc5, 0xb2, 0xfd, 0xae, 0xef, 0xf3, 0x17, 0xf1, 0x57, 0xe1
                ],
                0x31323334
            ),
            0x5a7c_50cf
        );
        assert_eq!(
            hashlittle(
                &[
                    0xe0, 0x97, 0x8c, 0x3f, 0x5f, 0xd5, 0xdf, 0x3d, 0x34, 0xf8, 0xc0, 0x82, 0x62
                ],
                0x31323334
            ),
            0x7f49_a662
        );
        assert_eq!(name_hash(b"hat"), 0x751c1036);
        assert_eq!(content_hash(b""), 0x43fa243a);
        let tails = [
            ("", 0x0fdf_f223),
            ("e1", 0x7eee_66cf),
            ("3b03", 0xf5c5_1f41),
            ("2e112a", 0x127c_8ba5),
            ("32b57908", 0x85f9_4d6f),
            ("0f08b1f7ed", 0x69a6_74f9),
            ("4c2e5d3a07f9", 0xe232_8b62),
            ("7f21ee232d178a", 0x9232_801e),
            ("209af6b5887f66e8", 0xb8b5_b3a4),
            ("092402aa49f2c1551b", 0x4a80_f91f),
            ("27fe53266e490db13848", 0xe083_7edc),
            ("9ce814d58d145a8b4f994f", 0xaafb_0f83),
            ("ed15c5b2fdaeeff317f157e1", 0x5a7c_50cf),
            ("e0978c3f5fd5df3d34f8c08262", 0x7f49_a662),
            ("b03750894fa5e42428ca6d189213", 0xc9f0_b48b),
            ("702ca29ceb218325da6733cb63eb78", 0x1222_0a05),
            ("b869d759689a1eb44efff1aa47431854", 0x7f40_b5b2),
        ];
        for (hex, expected) in tails {
            let data = (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                hashlittle(&data, 0x3132_3334),
                expected,
                "tail length {}",
                data.len()
            );
        }
        let long = concat!(
            "f18ea15521e075e27f22884f8c4ac96f664a6cafd300b161720809b9e17905d4d8fed7a97ff89cf0080a953fe77dcad66b15dcc839d35b5925bc577520c210",
            "41890eb01e8c0917d2f60f935d76fa1f967ce4ea598652d59f0495a6695e109dff09d953dead923028d6ab13d1e25dcf36a96133ca2da24025a9f6862720e6",
            "05b4126e37e45b1588cc9e10acaf6c2c7c329908222b0e98b0dc09c8e92c6f28b2abb4c6b5300f4244e6b740311f885110adfc2adeb809d7a1625475d857dd",
            "ff47c2f3c59c8d8449b743859f7f97554fe91d85d6bc19b20413659c61f3c690a1c4d48be41cab8363a130cebabada97c7d1908356e8b7665d1fc1e84379c0",
            "9bbed28272a7e2a03f54805acfe6858de62e54e9fb181d9a6cc243d224604e94be955cdff01a7bf4c831511b3a314b423ff1bd350a296945a0c183d896b8e5",
            "7520a0186899cdbffc9c83555ea20a14afa8b8e83edf2996d536a545c989f615c003d00e712020bebafa141e0009703dc58e768298202695aaafd9f313e9d5",
            "a66c6a668b06d20fef038bdd071bd0d8552de47813743086046bccc40d604931ad34292f3be71067cecb66a8ac1d1fd6d1e372e3e7b9a792f93889fbb8efa6",
            "354a5e368cd295e9898b2d17f4a5e3331c7e106a2e31cccbbba9308b6447b2ba8ef714ee15d90d6438985c87db5195fc232764afe931ec39fafec8909525d6",
            "72119d4d"
        );
        let data = (0..long.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&long[at..at + 2], 16).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(data.len(), 508);
        assert_eq!(hashlittle(&data, 0x3132_3334), 0xd2fa_036c);
    }
    #[test]
    fn structures() {
        let s = build_superblock();
        assert_eq!(u32::from_be_bytes(s[0..4].try_into().unwrap()), 0x656b4653);
        assert_eq!(
            hashlittle(&s[..0x1fc], 0x31323334),
            u32::from_be_bytes(s[0x1fc..].try_into().unwrap())
        );
        assert_eq!(
            crc32_ieee_raw(
                crc32_ieee_raw(0xffffffff, &build_factory_table_record()[8..12]),
                &build_factory_table_record()[4..8]
            ),
            0xdebb20e3
        );
        assert_eq!(build_boot_config_record().len(), 256);
    }
    #[test]
    fn directory_and_native() {
        let d = build_directory(
            2,
            2,
            &[
                DirEntry {
                    id: 3,
                    name: b"hat2".to_vec(),
                    kind: 0,
                },
                DirEntry {
                    id: 4,
                    name: b"hat10".to_vec(),
                    kind: 0,
                },
            ],
        )
        .unwrap();
        assert_eq!(d.contents.len(), PAGE);
        assert_eq!(u16::from_be_bytes(d.listing[..2].try_into().unwrap()), 4);
        assert!(
            build_directory(
                2,
                2,
                &[DirEntry {
                    id: 3,
                    name: vec![0; 256],
                    kind: 0
                }]
            )
            .is_err()
        );
        let n = build_native_sample(&[0, 1, 0, 2], true).unwrap();
        assert_eq!((n.len(), n[1], n[0x14]), (0x54, 1, 0x7f));
        assert!(build_native_sample(&[0], false).is_err());
    }
    #[test]
    fn natural_sort_handles_unbounded_digit_runs_and_stable_ties() {
        let zeros = format!("item{}", "0".repeat(100));
        let one = format!("item{}1", "0".repeat(99));
        let huge_low = format!("item1{}", "0".repeat(98));
        let huge_high = format!("item9{}", "0".repeat(98));
        assert_eq!(nat(zeros.as_bytes(), one.as_bytes()), Ordering::Less);
        assert_eq!(
            nat(huge_low.as_bytes(), huge_high.as_bytes()),
            Ordering::Less
        );
        assert_eq!(
            nat(b"item0000000000000000000000000000000000000002x", b"item2x"),
            Ordering::Equal
        );
        let d = build_directory(
            2,
            2,
            &[
                DirEntry {
                    id: 3,
                    name: b"item0002".to_vec(),
                    kind: ATTR_FILE,
                },
                DirEntry {
                    id: 4,
                    name: b"item2".to_vec(),
                    kind: ATTR_FILE,
                },
            ],
        )
        .unwrap();
        let first = u32::from_be_bytes(d.listing[28..32].try_into().unwrap());
        assert_eq!(first, 24);
    }
    #[test]
    fn project_header_rejects_partial_sequence_field() {
        assert!(build_project_header(0x100, 7, 9).is_ok());
        assert!(build_project_header(0x110, 7, 9).is_ok());
        for size in 0x105..0x108 {
            assert_eq!(
                build_project_header(size, 7, 9),
                Err(FormatError::InvalidProjectHeaderSize)
            );
        }
    }
}
