//! ADSP-2156x datasheet Rev D Tables 2--6, pp.9--10.
//! Access space is supplied by the instruction, never inferred from width.
#[inline(always)]
pub fn normal_word_to_byte(address: i128) -> Option<i128> {
    // Every range below lies in [0x90000, 0x18000000), none in
    // [0xe8000, 0x4000000): most data addresses fail here at once
    // (tools/sharc_core/addressing.py does the same).
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
