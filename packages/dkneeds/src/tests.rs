extern crate std;

use super::*;
use std::vec;
use std::vec::Vec;

fn put(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

/// A file with this image, bss, register table and DSP section (both may be empty), laid out like the packer's: header, register table, DSP
/// section, image. The tables the loader reads (relocations, imports, slots) are empty.
fn dkm(image: u32, bss: u32, reg: &[u8], dsp: &[u8]) -> Vec<u8> {
    let reg_off = HEADER as usize;
    let dsp_off = (reg_off + reg.len()).div_ceil(16) * 16;
    let image_off = (dsp_off + dsp.len()).div_ceil(16) * 16;
    let total = image_off + image as usize;
    let mut f = vec![0u8; total];
    put(&mut f, 0, MAGIC);
    put(&mut f, 4, FORMAT);
    put(&mut f, 8, total as u32);
    put(&mut f, 0x0c, HEADER);
    put(&mut f, H_IMAGE_OFF as usize, image_off as u32);
    put(&mut f, H_IMAGE_SIZE as usize, image);
    put(&mut f, H_BSS as usize, bss);
    put(&mut f, H_ALIGN as usize, 4);
    for at in [H_RELOC_OFF, H_IMPORT_OFF, H_SLOT_OFF] {
        put(&mut f, at as usize, HEADER);
    }
    put(&mut f, H_REG_OFF as usize, reg_off as u32);
    put(&mut f, H_REG_SIZE as usize, reg.len() as u32);
    put(&mut f, H_DSP_OFF as usize, dsp_off as u32);
    put(&mut f, H_DSP_SIZE as usize, dsp.len() as u32);
    f[reg_off..reg_off + reg.len()].copy_from_slice(reg);
    f[dsp_off..dsp_off + dsp.len()].copy_from_slice(dsp);
    f
}

/// A register table with these kinds in order.
fn register(kinds: &[u32]) -> Vec<u8> {
    let mut t = vec![0u8; REG_HEADER as usize];
    put(&mut t, 0, REG_MAGIC);
    put(&mut t, 4, 1);
    put(&mut t, 12, kinds.len() as u32);
    for &k in kinds {
        let len = if k == KIND_PARAM {
            LEN_PARAM
        } else if k == KIND_MACHINE {
            LEN_MACHINE
        } else if k == KIND_EFFECT {
            LEN_EFFECT
        } else if k == KIND_ACTION {
            LEN_ACTION
        } else {
            LEN_PAGE
        } as usize;
        let at = t.len();
        t.resize(at + len, 0);
        put(&mut t, at, k);
        put(&mut t, at + 4, len as u32);
    }
    t
}

/// A DSP section (no relocations or imports) of `image` bytes and `bss`, version `version`, and voice records `(state_bytes, voices)`.
fn dsp_section(version: u32, image: u32, bss: u32, voices: &[(u32, u32)]) -> Vec<u8> {
    let stride = if version == 1 {
        16
    } else if version == 2 {
        48
    } else {
        64
    };
    let voice_off = DSP_HEADER as usize;
    let image_off = voice_off + stride * voices.len();
    let mut s = vec![0u8; image_off + image as usize];
    put(&mut s, 0, DSP_MAGIC);
    put(&mut s, 4, version);
    put(&mut s, 8, 1);
    put(&mut s, 0x0c, image);
    put(&mut s, 0x10, bss);
    put(&mut s, 0x14, 16);
    put(&mut s, 0x18, image_off as u32);
    put(&mut s, 0x1c, voice_off as u32);
    put(&mut s, 0x24, voice_off as u32);
    put(&mut s, 0x2c, voice_off as u32);
    put(&mut s, 0x30, voices.len() as u32);
    for (i, &(state, count)) in voices.iter().enumerate() {
        let at = voice_off + stride * i;
        put(&mut s, at, 17 + i as u32);
        if version == 3 {
            put(&mut s, at + 12, state);
            put(&mut s, at + 52, count);
        }
    }
    s
}

fn of(f: &[u8]) -> Result<Needs, Error> {
    needs_of(&mut &f[..])
}

#[test]
fn a_coldfire_only_module_takes_an_arena_block() {
    // image 12 + bss 20 = 32: two units of 16, and the header
    let n = of(&dkm(12, 20, &[], &[])).unwrap();
    assert_eq!(n.arena, 32 + ARENA_HDR);
    assert_eq!(
        (
            n.arena_xfer,
            n.dsp_area,
            n.dsp_align,
            n.machine_rows,
            n.voice_rows,
            n.param_rows,
            n.page_rows,
            n.fx_slots
        ),
        (0, 0, 0, 0, 0, 0, 0, 0)
    );
    assert_eq!(n.file_size as usize, dkm(12, 20, &[], &[]).len());
    // rounded up to 16
    assert_eq!(of(&dkm(4, 0, &[], &[])).unwrap().arena, 16 + ARENA_HDR);
    assert_eq!(of(&dkm(17, 0, &[], &[])).unwrap().arena, 32 + ARENA_HDR);
}

#[test]
fn register_records_are_counted_by_kind() {
    let reg = register(&[KIND_PARAM, KIND_PARAM, KIND_MACHINE, KIND_PAGE, KIND_PARAM]);
    let n = of(&dkm(4, 0, &reg, &[])).unwrap();
    assert_eq!((n.param_rows, n.machine_rows, n.page_rows), (3, 1, 1));
    // an effect's page takes no row of its own
    let reg = register(&[KIND_EFFECT, KIND_PARAM]);
    let n = of(&dkm(4, 0, &reg, &[])).unwrap();
    assert_eq!((n.param_rows, n.machine_rows, n.page_rows), (1, 0, 0));
    // a click action takes no row either
    let reg = register(&[KIND_PARAM, KIND_ACTION]);
    let n = of(&dkm(4, 0, &reg, &[])).unwrap();
    assert_eq!((n.param_rows, n.machine_rows, n.page_rows), (1, 0, 0));
    // an empty table (a header only) counts nothing
    let n = of(&dkm(4, 0, &register(&[]), &[])).unwrap();
    assert_eq!((n.param_rows, n.machine_rows, n.page_rows), (0, 0, 0));
    // the most records
    let n = of(&dkm(
        4,
        0,
        &register(&[KIND_PARAM; MAX_RECORDS as usize]),
        &[],
    ))
    .unwrap();
    assert_eq!(n.param_rows, MAX_RECORDS);
}

#[test]
fn a_dsp_section_takes_a_range_a_block_and_voice_rows() {
    // image 8, bss 100, three states of 20 (24 each) of one voice record: 108 -> 112, + 72 = 184, granule 192
    let f = dkm(4, 0, &[], &dsp_section(3, 8, 100, &[(20, 3)]));
    let n = of(&f).unwrap();
    assert_eq!(
        (n.dsp_area, n.dsp_align, n.voice_rows, n.arena_xfer),
        (192, 16, 1, 16 + ARENA_HDR)
    );
    // version 2 voices have no state: 108 -> 112 -> granule 128
    let n = of(&dkm(4, 0, &[], &dsp_section(2, 8, 100, &[(0, 0), (0, 0)]))).unwrap();
    assert_eq!((n.dsp_area, n.voice_rows), (128, 2));
    // version 1 too
    let n = of(&dkm(4, 0, &[], &dsp_section(1, 8, 0, &[(0, 0)]))).unwrap();
    assert_eq!((n.dsp_area, n.voice_rows), (64, 1));
    // two voice records of version 3: the states of each, in turn
    let n = of(&dkm(4, 0, &[], &dsp_section(3, 64, 0, &[(8, 2), (12, 1)]))).unwrap();
    assert_eq!(n.dsp_area, (64u32 + 16 + 16).div_ceil(GRANULE) * GRANULE);
    // the DSP image's block is the image rounded to 16, and the header
    assert_eq!(
        of(&dkm(4, 0, &[], &dsp_section(2, 20, 0, &[(0, 0)])))
            .unwrap()
            .arena_xfer,
        32 + ARENA_HDR
    );
    // the placement can lose up to align - 8 bytes
    let n = of(&f).unwrap();
    assert_eq!(n.dsp_need(), 192 + 8);
    assert_eq!(Needs::default().dsp_need(), 0);
}

/// A version 4 DSP section: one voice record of version 3 form is not needed, `fx` effect records `(state_bytes)` follow an empty voice table.
fn dsp_section_v4(image: u32, bss: u32, fx: &[u32]) -> Vec<u8> {
    let fx_off = DSP_HEADER as usize;
    let image_off = fx_off + FX_LEN as usize * fx.len();
    let mut s = vec![0u8; image_off + image as usize];
    put(&mut s, 0, DSP_MAGIC);
    put(&mut s, 4, 4);
    put(&mut s, 8, 1);
    put(&mut s, 0x0c, image);
    put(&mut s, 0x10, bss);
    put(&mut s, 0x14, 16);
    put(&mut s, 0x18, image_off as u32);
    put(&mut s, 0x1c, fx_off as u32);
    put(&mut s, 0x24, fx_off as u32);
    put(&mut s, 0x2c, fx_off as u32);
    put(&mut s, 0x34, fx_off as u32);
    put(&mut s, 0x38, fx.len() as u32);
    for (i, &state) in fx.iter().enumerate() {
        put(&mut s, fx_off + FX_LEN as usize * i + 8, state);
    }
    s
}

#[test]
fn effect_records_take_effect_slots_and_their_state() {
    // image 8, bss 100 (108 -> 112), one effect with 20 bytes of state (24): 136, granule 192; no voice row
    let n = of(&dkm(4, 0, &[], &dsp_section_v4(8, 100, &[20]))).unwrap();
    assert_eq!(
        (n.fx_slots, n.voice_rows, n.dsp_area, n.arena_xfer),
        (1, 0, 192, 16 + ARENA_HDR)
    );
    // an effect with no state adds none; three effects count three slots and add their states in turn
    let n = of(&dkm(4, 0, &[], &dsp_section_v4(8, 0, &[0]))).unwrap();
    assert_eq!((n.fx_slots, n.dsp_area), (1, 64));
    let n = of(&dkm(4, 0, &[], &dsp_section_v4(64, 0, &[8, 12, 16]))).unwrap();
    assert_eq!(n.fx_slots, 3);
    assert_eq!(n.dsp_area, (64u32 + 8 + 16 + 16).div_ceil(GRANULE) * GRANULE);
    // versions before 4 take no slot
    assert_eq!(of(&dkm(4, 0, &[], &dsp_section(3, 8, 0, &[(0, 0)]))).unwrap().fx_slots, 0);
    // more than DK_FX_MAX records, a table outside the section and an absurd state are refused
    let f = dkm(4, 0, &[], &dsp_section_v4(8, 0, &[0, 0, 0, 0, 0]));
    assert_eq!(of(&f), Err(Error::Dsp));
    let mut bad = dsp_section_v4(8, 0, &[0]);
    put(&mut bad, 0x34, 0x1000);
    assert_eq!(of(&dkm(4, 0, &[], &bad)), Err(Error::Dsp));
    assert_eq!(
        of(&dkm(4, 0, &[], &dsp_section_v4(8, 0, &[MAX_STATE_BYTES + 4]))),
        Err(Error::Dsp)
    );
}

#[test]
fn the_fields_have_the_capacity_names() {
    let n = Needs {
        file_size: 1,
        arena: 2,
        arena_xfer: 3,
        dsp_area: 4,
        dsp_align: 5,
        machine_rows: 6,
        voice_rows: 7,
        param_rows: 8,
        page_rows: 9,
        fx_slots: 10,
    };
    let f = n.fields();
    assert_eq!(f[0], ("ARENA", 2));
    assert_eq!(f[8], ("FX_SLOTS", 10));
    assert_eq!(
        f.iter().map(|x| x.1).sum::<u32>(),
        2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10
    );
}

#[test]
fn a_file_that_is_not_a_dkm_has_no_needs() {
    let good = dkm(4, 0, &[], &[]);
    assert_eq!(of(&[]), Err(Error::Header));
    assert_eq!(of(&good[..HEADER as usize - 1]), Err(Error::Header));
    let mut f = good.clone();
    f[0] ^= 1;
    assert_eq!(of(&f), Err(Error::Header));
    let mut f = good.clone();
    put(&mut f, 4, 2); // another format
    assert_eq!(of(&f), Err(Error::Header));
    let mut f = good.clone();
    put(&mut f, 8, good.len() as u32 + 1); // the size field is not the file's
    assert_eq!(of(&f), Err(Error::Header));
    let mut f = good.clone();
    put(&mut f, 0x0c, 0xd0); // another header size
    assert_eq!(of(&f), Err(Error::Header));
    // a file cut short: the size field no longer matches
    assert_eq!(of(&good[..good.len() - 1]), Err(Error::Header));
}

#[test]
fn absurd_sizes_and_counts_are_refused_without_a_panic() {
    let reg = register(&[KIND_PARAM, KIND_MACHINE]);
    let dsp = dsp_section(3, 8, 0, &[(20, 3)]);
    let good = dkm(16, 0, &reg, &dsp);
    assert!(of(&good).is_ok());
    let edit = |at: usize, v: u32| {
        let mut f = good.clone();
        put(&mut f, at, v);
        f
    };
    let dsp_off = u32::from_be_bytes(
        good[H_DSP_OFF as usize..H_DSP_OFF as usize + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let reg_off = HEADER as usize;
    // header
    assert_eq!(of(&edit(H_IMAGE_SIZE as usize, 0)), Err(Error::Sizes));
    assert_eq!(
        of(&edit(H_IMAGE_SIZE as usize, good.len() as u32 + 1)),
        Err(Error::Sizes)
    );
    assert_eq!(
        of(&edit(H_IMAGE_SIZE as usize, u32::MAX)),
        Err(Error::Sizes)
    );
    // image + bss overflows
    let mut f = edit(H_IMAGE_SIZE as usize, 16);
    put(&mut f, H_BSS as usize, u32::MAX);
    assert_eq!(of(&f), Err(Error::Sizes));
    // a bss that is not an overflow is a need, not an error: the device's arena decides
    put(&mut f, H_BSS as usize, 0x4000_0000);
    assert_eq!(of(&f).unwrap().arena, 0x4000_0010 + ARENA_HDR);
    assert_eq!(of(&edit(H_ALIGN as usize, 0)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_ALIGN as usize, 32)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_ALIGN as usize, 12)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_RELOC_N as usize, 4097)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_RELOC_N as usize, u32::MAX)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_IMPORT_N as usize, 65)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_SLOT_N as usize, 9)), Err(Error::Sizes));
    assert_eq!(
        of(&edit(H_IMAGE_OFF as usize, u32::MAX - 15)),
        Err(Error::Sizes)
    );
    assert_eq!(of(&edit(H_REG_OFF as usize, u32::MAX)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_REG_SIZE as usize, u32::MAX)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_DSP_OFF as usize, u32::MAX)), Err(Error::Sizes));
    assert_eq!(of(&edit(H_DSP_SIZE as usize, u32::MAX)), Err(Error::Sizes));
    // a DSP section that runs past the file (truncated): its size field says more than the file has
    assert_eq!(
        of(&edit(H_DSP_SIZE as usize, good.len() as u32)),
        Err(Error::Sizes)
    );
    // register table
    assert_eq!(of(&edit(reg_off, 0)), Err(Error::Register));
    assert_eq!(of(&edit(reg_off + 4, 2)), Err(Error::Register));
    assert_eq!(
        of(&edit(reg_off + 12, MAX_RECORDS + 1)),
        Err(Error::Register)
    );
    assert_eq!(of(&edit(reg_off + 12, u32::MAX)), Err(Error::Register));
    assert_eq!(of(&edit(reg_off + 12, 1)), Err(Error::Register)); // the table is longer than one record
    assert_eq!(of(&edit(reg_off + 12, 3)), Err(Error::Register)); // and shorter than three
    assert_eq!(of(&edit(reg_off + 16, 9)), Err(Error::Register)); // a kind that does not exist
    assert_eq!(of(&edit(reg_off + 20, LEN_PARAM + 4)), Err(Error::Register)); // a length that is not the kind's
    assert_eq!(of(&edit(reg_off + 20, 0)), Err(Error::Register));
    assert_eq!(of(&edit(reg_off + 20, u32::MAX)), Err(Error::Register));
    assert_eq!(of(&edit(H_REG_SIZE as usize, 12)), Err(Error::Register)); // too short for its header
                                                                          // DSP section
    let d = |at: usize, v: u32| edit(dsp_off + at, v);
    assert_eq!(of(&d(0, 0)), Err(Error::Dsp));
    assert_eq!(of(&d(4, 0)), Err(Error::Dsp));
    assert_eq!(of(&d(4, 5)), Err(Error::Dsp));
    assert_eq!(of(&d(0x0c, 0)), Err(Error::Dsp));
    assert_eq!(of(&d(0x0c, 10)), Err(Error::Dsp)); // not a multiple of 4
    assert_eq!(of(&d(0x0c, u32::MAX - 3)), Err(Error::Dsp));
    assert_eq!(of(&d(0x10, 6)), Err(Error::Dsp)); // bss
    assert_eq!(of(&d(0x14, 4)), Err(Error::Dsp));
    assert_eq!(of(&d(0x14, 128)), Err(Error::Dsp));
    assert_eq!(of(&d(0x14, 24)), Err(Error::Dsp));
    assert_eq!(of(&d(0x18, u32::MAX)), Err(Error::Dsp));
    assert_eq!(of(&d(0x20, MAX_RELOCS + 1)), Err(Error::Dsp));
    assert_eq!(of(&d(0x20, u32::MAX)), Err(Error::Dsp));
    assert_eq!(of(&d(0x28, MAX_IMPORTS + 1)), Err(Error::Dsp));
    assert_eq!(of(&d(0x2c, u32::MAX)), Err(Error::Dsp));
    assert_eq!(of(&d(0x30, MAX_VOICES + 1)), Err(Error::Dsp));
    assert_eq!(of(&d(0x30, u32::MAX)), Err(Error::Dsp));
    assert_eq!(of(&d(0x30, 8)), Err(Error::Dsp)); // eight voice records do not fit this section
    let v = DSP_HEADER as usize;
    assert_eq!(of(&d(v + 12, MAX_STATE_BYTES + 1)), Err(Error::Dsp)); // state bytes
    assert_eq!(of(&d(v + 12, u32::MAX)), Err(Error::Dsp));
    assert_eq!(of(&d(v + 52, MAX_STATE_VOICES + 1)), Err(Error::Dsp)); // voices
    assert_eq!(of(&d(v + 52, u32::MAX)), Err(Error::Dsp));
    // the largest state and the most voices: a big range, not an error, and no overflow
    let mut f = d(v + 12, MAX_STATE_BYTES);
    put(&mut f, dsp_off + v + 52, MAX_STATE_VOICES);
    assert_eq!(
        of(&f).unwrap().dsp_area,
        MAX_STATE_BYTES * MAX_STATE_VOICES + GRANULE
    );
    // the ranges of several voice records add up past 32 bits
    let big = dsp_section(3, 8, 0, &[(MAX_STATE_BYTES, MAX_STATE_VOICES); 8]);
    assert_eq!(of(&dkm(4, 0, &[], &big)), Err(Error::Dsp));
}

#[test]
fn every_truncation_and_every_byte_edit_ends_in_a_value_or_an_error() {
    // not a proof: the file is cut at every length and each of its first 0x200 bytes takes a few other values; nothing panics, nothing reads past
    // the end, and a cut file is refused (its size field no longer matches)
    struct Probe<'a> {
        f: &'a [u8],
        past: bool,
    }
    impl Bytes for Probe<'_> {
        fn size(&self) -> u32 {
            self.f.len() as u32
        }
        fn read(&mut self, at: u32, out: &mut [u8]) -> bool {
            let end = at as usize + out.len();
            if end > self.f.len() {
                self.past = true;
                return false;
            }
            out.copy_from_slice(&self.f[at as usize..end]);
            true
        }
    }
    let reg = register(&[KIND_PARAM, KIND_MACHINE, KIND_PAGE]);
    let good = dkm(32, 8, &reg, &dsp_section(3, 24, 8, &[(16, 2), (8, 1)]));
    assert!(of(&good).is_ok());
    for cut in 0..good.len() {
        let mut p = Probe {
            f: &good[..cut],
            past: false,
        };
        assert!(needs_of(&mut p).is_err(), "cut at {cut}");
        assert!(!p.past, "a read past the end at {cut}");
    }
    let mut x = 0x2545_f491u32;
    for at in 0..0x200.min(good.len()) {
        for _ in 0..8 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let mut f = good.clone();
            f[at] = x as u8;
            let mut p = Probe { f: &f, past: false };
            let _ = needs_of(&mut p);
            assert!(!p.past, "a read past the end after editing byte {at}");
        }
    }
}

#[test]
fn a_source_that_cannot_be_read_is_unreadable() {
    struct Dead(u32);
    impl Bytes for Dead {
        fn size(&self) -> u32 {
            self.0
        }
        fn read(&mut self, _: u32, _: &mut [u8]) -> bool {
            false
        }
    }
    assert_eq!(needs_of(&mut Dead(1000)), Err(Error::Unreadable));
    // too short to be a file: refused before any read
    assert_eq!(needs_of(&mut Dead(10)), Err(Error::Header));
    assert_eq!(Error::Unreadable.text(), "the file cannot be read");
}

#[test]
fn the_reads_stay_in_the_tables_the_loader_reads() {
    // a card feeds the file by sectors: the answer needs the header, the register table's record heads and the DSP section's header and voice
    // table, not the image
    struct Log<'a> {
        f: &'a [u8],
        reads: Vec<(u32, usize)>,
    }
    impl Bytes for Log<'_> {
        fn size(&self) -> u32 {
            self.f.len() as u32
        }
        fn read(&mut self, at: u32, out: &mut [u8]) -> bool {
            self.reads.push((at, out.len()));
            out.copy_from_slice(&self.f[at as usize..at as usize + out.len()]);
            true
        }
    }
    let reg = register(&[KIND_PARAM, KIND_MACHINE]);
    let dsp = dsp_section(3, 4096, 0, &[(8, 1)]);
    let f = dkm(4096, 0, &reg, &dsp);
    let image_off = u32::from_be_bytes(
        f[H_IMAGE_OFF as usize..H_IMAGE_OFF as usize + 4]
            .try_into()
            .unwrap(),
    );
    let mut l = Log {
        f: &f,
        reads: vec![],
    };
    needs_of(&mut l).unwrap();
    assert!(
        l.reads
            .iter()
            .all(|&(at, n)| at as usize + n <= image_off as usize),
        "{:?}",
        l.reads
    );
    assert!(l.reads.len() < 60, "{} reads", l.reads.len());
}

#[test]
fn sample_bank_v5_counts_owned_bss_and_rejects_bad_records() {
    let mut dsp = dsp_section_v4(16, 32, &[8]);
    let bank_off = DSP_HEADER as usize + FX_LEN as usize;
    dsp.splice(bank_off..bank_off, [0u8; 32]);
    put(&mut dsp, 4, 5);
    put(&mut dsp, 0x18, bank_off as u32 + 32);
    put(&mut dsp, 0x3c, bank_off as u32);
    for (i, w) in [1u32, 7, 16, 8, 0, 0, 0, 0].iter().enumerate() {
        dsp[bank_off + i * 4..bank_off + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    let n = of(&dkm(4, 0, &[], &dsp)).unwrap();
    assert_eq!((n.fx_slots, n.dsp_area), (1, 64));
    for (word, value) in [
        (0, 2u32),
        (2, 0),
        (2, u32::MAX - 3),
        (3, 0),
        (3, 9),
        (4, 4),
        (5, 1),
    ] {
        let mut bad = dsp.clone();
        bad[bank_off + word * 4..bank_off + word * 4 + 4].copy_from_slice(&value.to_le_bytes());
        assert_eq!(of(&dkm(4, 0, &[], &bad)), Err(Error::Dsp));
    }
    let mut bad = dsp.clone();
    put(&mut bad, 0x3c, DSP_HEADER);
    assert_eq!(of(&dkm(4, 0, &[], &bad)), Err(Error::Dsp));
    let image_off = bank_off + 32;
    dsp[image_off] = 1;
    assert_eq!(of(&dkm(4, 0, &[], &dsp)), Err(Error::Dsp));
}
