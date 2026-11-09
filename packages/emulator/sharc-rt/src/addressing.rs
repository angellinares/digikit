//! ADSP-2156x datasheet Rev D Tables 2--6, pp.9--10.
//! Access space is supplied by the instruction, never inferred from width.
/// May A be mapped by `normal_word_map`? Every range of the table lies in
/// [0x90000, 0xe8000) or [0x4000000, 0x18000000) and every boundary is a
/// multiple of 0x2000, so two small-immediate band tests on A >> 13 decide
/// it (most data addresses fail both at once).
#[inline(always)]
fn nw_band(a: u32) -> bool {
    let h = a >> 13;
    h.wrapping_sub(0x48) < 0x2c || h.wrapping_sub(0x2000) < 0xa000
}

/// The mapped byte address of a normal-word address, or None. The range
/// table is one out-of-line function, so block code carries it once, not at
/// every memory access.
#[inline(always)]
pub fn normal_word_to_byte(address: i128) -> Option<i128> {
    if (address as u128) >> 32 != 0 {
        return None;
    }
    let a = address as u32;
    if !nw_band(a) {
        return None;
    }
    match normal_word_map(a) {
        0 => None,
        mapped => Some(mapped as i128),
    }
}

/// normal_word_to_byte on a 32-bit address, for an access that falls back to
/// the address itself when no range maps it (the plain-RAM fast path of
/// rt/bnd.rs): the mapped byte address, else A.
#[inline(always)]
pub fn normal_word_or_same(a: u32) -> u32 {
    if !nw_band(a) {
        return a;
    }
    match normal_word_map(a) {
        0 => a,
        mapped => mapped,
    }
}

/// The table behind normal_word_to_byte for an address in [0x90000,
/// 0x18000000) outside [0xe8000, 0x4000000): the mapped byte address (never
/// 0), or 0 when no range holds it.
#[inline(never)]
fn normal_word_map(a: u32) -> u32 {
    match a {
        0x90000..0x9c000 => 0x28240000 + (a - 0x90000) * 4,
        0xb0000..0xbc000 => 0x282c0000 + (a - 0xb0000) * 4,
        0xc0000..0xc8000 => 0x28300000 + (a - 0xc0000) * 4,
        0xe0000..0xe8000 => 0x28380000 + (a - 0xe0000) * 4,
        0x4000000..0x8000000 => 0x60000000 + (a - 0x4000000) * 4,
        0x8000000..0x8046000 => 0x20000000 + (a - 0x8000000) * 4,
        0xa090000..0xa09c000 => 0x28240000 + (a - 0xa090000) * 4,
        0xa0b0000..0xa0bc000 => 0x282c0000 + (a - 0xa0b0000) * 4,
        0xa0c0000..0xa0c8000 => 0x28300000 + (a - 0xa0c0000) * 4,
        0xa0e0000..0xa0e8000 => 0x28380000 + (a - 0xa0e0000) * 4,
        0x10000000..0x18000000 => 0x80000000 + (a - 0x10000000) * 4,
        _ => 0,
    }
}
#[inline(always)]
pub fn byte_to_normal_word(address: i128) -> Option<i128> {
    if (0x240000..0x270000).contains(&address) {
        return Some(0x90000 + (address - 0x240000) * 1 / 4);
    }
    if (0x2c0000..0x2f0000).contains(&address) {
        return Some(0xb0000 + (address - 0x2c0000) * 1 / 4);
    }
    if (0x300000..0x320000).contains(&address) {
        return Some(0xc0000 + (address - 0x300000) * 1 / 4);
    }
    if (0x380000..0x3a0000).contains(&address) {
        return Some(0xe0000 + (address - 0x380000) * 1 / 4);
    }
    if (0x20000000..0x20118000).contains(&address) {
        return Some(0x8000000 + (address - 0x20000000) * 1 / 4);
    }
    if (0x28240000..0x28270000).contains(&address) {
        return Some(0xa090000 + (address - 0x28240000) * 1 / 4);
    }
    if (0x282c0000..0x282f0000).contains(&address) {
        return Some(0xa0b0000 + (address - 0x282c0000) * 1 / 4);
    }
    if (0x28300000..0x28320000).contains(&address) {
        return Some(0xa0c0000 + (address - 0x28300000) * 1 / 4);
    }
    if (0x28380000..0x283a0000).contains(&address) {
        return Some(0xa0e0000 + (address - 0x28380000) * 1 / 4);
    }
    if (0x60000000..0x70000000).contains(&address) {
        return Some(0x4000000 + (address - 0x60000000) * 1 / 4);
    }
    if (0x80000000..0xa0000000).contains(&address) {
        return Some(0x10000000 + (address - 0x80000000) * 1 / 4);
    }
    None
}
#[inline(always)]
pub fn normal_word_to_architectural_byte(address: i128) -> Option<i128> {
    normal_word_to_byte(address).map(|mapped| {
        if address < 0x100000 {
            mapped - 0x28000000
        } else {
            mapped
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The table as it was before the range walk moved out of line.
    fn reference_normal_word_to_byte(address: i128) -> Option<i128> {
        // Every range below lies in [0x90000, 0x18000000), none in
        // [0xe8000, 0x4000000): most data addresses fail here at once
        // (tools/sharc_dev/sharc_core/addressing.py does the same).
        if !(0x90000..0x1800_0000).contains(&address) || (0xe8000..0x400_0000).contains(&address) {
            return None;
        }
        if (0x90000..0x9c000).contains(&address) {
            return Some(0x28240000 + (address - 0x90000) * 4 / 1);
        }
        if (0xb0000..0xbc000).contains(&address) {
            return Some(0x282c0000 + (address - 0xb0000) * 4 / 1);
        }
        if (0xc0000..0xc8000).contains(&address) {
            return Some(0x28300000 + (address - 0xc0000) * 4 / 1);
        }
        if (0xe0000..0xe8000).contains(&address) {
            return Some(0x28380000 + (address - 0xe0000) * 4 / 1);
        }
        if (0x4000000..0x8000000).contains(&address) {
            return Some(0x60000000 + (address - 0x4000000) * 4 / 1);
        }
        if (0x8000000..0x8046000).contains(&address) {
            return Some(0x20000000 + (address - 0x8000000) * 4 / 1);
        }
        if (0xa090000..0xa09c000).contains(&address) {
            return Some(0x28240000 + (address - 0xa090000) * 4 / 1);
        }
        if (0xa0b0000..0xa0bc000).contains(&address) {
            return Some(0x282c0000 + (address - 0xa0b0000) * 4 / 1);
        }
        if (0xa0c0000..0xa0c8000).contains(&address) {
            return Some(0x28300000 + (address - 0xa0c0000) * 4 / 1);
        }
        if (0xa0e0000..0xa0e8000).contains(&address) {
            return Some(0x28380000 + (address - 0xa0e0000) * 4 / 1);
        }
        if (0x10000000..0x18000000).contains(&address) {
            return Some(0x80000000 + (address - 0x10000000) * 4 / 1);
        }
        None
    }

    #[test]
    fn normal_word_to_byte_matches_the_reference() {
        let mut checked = 0u64;
        let mut check = |a: i128| {
            assert_eq!(
                normal_word_to_byte(a),
                reference_normal_word_to_byte(a),
                "{a:#x}"
            );
            checked += 1;
        };
        // Every range boundary and its neighbours, then a stride over
        // the whole interesting span and the 32-bit edges.
        let bounds: [i128; 24] = [
            0x90000,
            0x9c000,
            0xb0000,
            0xbc000,
            0xc0000,
            0xc8000,
            0xe0000,
            0xe8000,
            0x4000000,
            0x8000000,
            0x8046000,
            0xa090000,
            0xa09c000,
            0xa0b0000,
            0xa0bc000,
            0xa0c0000,
            0xa0c8000,
            0xa0e0000,
            0xa0e8000,
            0x10000000,
            0x18000000,
            0x7fff_ffff,
            0x8000_0000,
            0xffff_ffff,
        ];
        for b in bounds {
            for d in -3..=3 {
                check(b + d);
            }
        }
        for a in (-0x1000i128..0x1900_0000).step_by(0x1f1) {
            check(a);
        }
        for a in (0x1900_0000i128..0x1_0000_1000).step_by(0x3_1337) {
            check(a);
        }
        check(-1);
        check(i128::MAX);
        check(i128::MIN);
        // The 32-bit form maps like the wide one, or leaves the address.
        for a in (0u32..0x1900_0000)
            .step_by(0x1f3)
            .chain(0xffff_f000..=u32::MAX)
        {
            let want = reference_normal_word_to_byte(a as i128).map_or(a, |m| m as u32);
            assert_eq!(normal_word_or_same(a), want, "{a:#x}");
        }
        for b in [
            0x90000u32,
            0x9c000,
            0xe7fff,
            0xe8000,
            0x3ff_ffff,
            0x400_0000,
            0x1800_0000,
        ] {
            for d in 0..3u32 {
                for a in [b.wrapping_add(d), b.wrapping_sub(d)] {
                    let want = reference_normal_word_to_byte(a as i128).map_or(a, |m| m as u32);
                    assert_eq!(normal_word_or_same(a), want, "{a:#x}");
                }
            }
        }
        assert!(checked > 100_000);
    }
}
