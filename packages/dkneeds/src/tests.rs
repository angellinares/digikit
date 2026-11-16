extern crate std;

use super::*;
use std::vec;
use std::vec::Vec;

fn put(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

/// A file with this image, bss, keys table and DSP section (both may be empty), laid out like the packer's: header, keys table, DSP
/// section, image. The tables the loader reads (relocations, imports) are empty.
fn dkm(image: u32, bss: u32, reg: &[u8], dsp: &[u8]) -> Vec<u8> {
    // a file always has a header table: with none given, the table of a module that needs and keeps nothing
    let none = register(&[]);
    let reg = if reg.is_empty() { &none[..] } else { reg };
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
    for at in [H_RELOC_OFF, H_IMPORT_OFF] {
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

use crate::{KIND_MACHINE, KIND_PAGE, KIND_PARAM};

/// A header table whose row counts are those of these kinds (one machine key per machine row).
fn register(kinds: &[u32]) -> Vec<u8> {
    let n = |k: u32| kinds.iter().filter(|&&x| x == k).count();
    let machines: Vec<u32> = (0..n(KIND_MACHINE) as u32).map(|i| 0x100 + i).collect();
    let rows = (
        n(KIND_MACHINE) as u32,
        n(KIND_PARAM) as u32,
        n(KIND_PAGE) as u32,
    );
    let mut t = vec![0u8; keys::keys_size(rows, 0, machines.len(), 0)];
    keys::write_keys(rows, 0, &machines, &[], &mut t).unwrap();
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
fn the_rows_come_from_the_keys_table() {
    let t = {
        let mut t = [0u8; 256];
        let size = keys::write_keys((2, 9, 3), 0x1b0, &[0x11, 0x22], &[0x33], &mut t).unwrap();
        t[..size].to_vec()
    };
    let n = of(&dkm(4, 0, &t, &[])).unwrap();
    assert_eq!((n.machine_rows, n.param_rows, n.page_rows), (2, 9, 3));
    // the record block is one more arena block: 0x1b0 payload bytes and the header
    let plain = of(&dkm(4, 0, &[], &[])).unwrap().arena;
    assert_eq!(n.arena, plain + arena_block(0x1b0).unwrap());
    // a module with no machine and no effect has the head only
    assert_eq!(of(&dkm(4, 0, &[], &[])).unwrap().machine_rows, 0);
    // a file without a table (the format of runtime 0.7 with no machine) is refused
    let mut old = dkm(4, 0, &[], &[]);
    put(&mut old, H_REG_SIZE as usize, 0);
    assert_eq!(of(&old), Err(Error::Register));
    // a table whose size is not the counts' size, is refused
    assert_eq!(of(&dkm(4, 0, &t[..t.len() - 4], &[])), Err(Error::Register));
}

#[test]
fn the_dsp_cost_comes_from_the_uses_of_the_keys_table() {
    let uses = [
        keys::Use {
            resource: crate::RES_DSP_COST_VOICE,
            count: 6239,
        },
        keys::Use {
            resource: crate::RES_DSP_COST_FX,
            count: 5282,
        },
    ];
    let table = keys::Table {
        uses: &uses,
        ..Default::default()
    };
    let mut t = vec![0u8; keys::table_size(&table)];
    keys::write_table(&table, &mut t).unwrap();
    let n = of(&dkm(4, 0, &t, &[])).unwrap();
    assert_eq!((n.dsp_cost_voice, n.dsp_cost_fx), (6239, 5282));
    // a module that lists none has no cost
    let n = of(&dkm(4, 0, &[], &[])).unwrap();
    assert_eq!((n.dsp_cost_voice, n.dsp_cost_fx), (0, 0));
}

#[test]
fn the_menu_reader_accepts_a_version_2_table() {
    let uses = [keys::Use {
        resource: crate::RES_MACHINE_ROWS,
        count: 3,
    }];
    let stock = [1u8, 2, 3];
    let table = keys::Table {
        uses: &uses,
        slots: &[keys::Slot {
            id: 1,
            flags: 0,
            entry: 0x40,
        }],
        provides: &[keys::Pair { a: 5, b: 1 }],
        bind: &[keys::Bind {
            site: 6,
            addr: 0x1000,
            tramp: 0x20,
            stock: &stock,
        }],
        ..Default::default()
    };
    let mut t = vec![0u8; keys::table_size(&table)];
    keys::write_table(&table, &mut t).unwrap();
    assert_eq!(t[5], 2);
    assert_eq!(of(&dkm(4, 0, &t, &[])).unwrap().machine_rows, 3);
    // another version is refused
    t[5] = 3;
    assert_eq!(of(&dkm(4, 0, &t, &[])), Err(Error::Register));
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
    assert_eq!(
        n.dsp_area,
        (64u32 + 8 + 16 + 16).div_ceil(GRANULE) * GRANULE
    );
    // versions before 4 take no slot
    assert_eq!(
        of(&dkm(4, 0, &[], &dsp_section(3, 8, 0, &[(0, 0)])))
            .unwrap()
            .fx_slots,
        0
    );
    // more than DK_FX_MAX records, a table outside the section and an absurd state are refused
    let f = dkm(4, 0, &[], &dsp_section_v4(8, 0, &[0, 0, 0, 0, 0]));
    assert_eq!(of(&f), Err(Error::Dsp));
    let mut bad = dsp_section_v4(8, 0, &[0]);
    put(&mut bad, 0x34, 0x1000);
    assert_eq!(of(&dkm(4, 0, &[], &bad)), Err(Error::Dsp));
    assert_eq!(
        of(&dkm(
            4,
            0,
            &[],
            &dsp_section_v4(8, 0, &[MAX_STATE_BYTES + 4])
        )),
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
        dsp_cost_voice: 11,
        dsp_cost_fx: 12,
    };
    let f = n.fields();
    assert_eq!(f[0], ("ARENA", 2));
    assert_eq!(f[8], ("FX_SLOTS", 10));
    assert_eq!((f[9], f[10]), (("DSP_COST_VOICE", 11), ("DSP_COST_FX", 12)));
    assert_eq!(
        f.iter().map(|x| x.1).sum::<u32>(),
        2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12
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
    // keys table
    assert_eq!(of(&edit(reg_off, 0)), Err(Error::Register)); // no magic: the table of runtime 0.7
    assert_eq!(
        of(&edit(reg_off + keys::HEAD + 4, u32::MAX)),
        Err(Error::Register)
    ); // the first entry is longer than the table
    assert_eq!(of(&edit(H_REG_SIZE as usize, 8)), Err(Error::Register)); // too short for its head
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
fn sample_track_record_v5_needs_kind_2_zero_blocks_and_no_overlap() {
    let mut dsp = dsp_section_v4(768, 0, &[8]);
    let record_off = DSP_HEADER as usize + FX_LEN as usize;
    dsp.splice(record_off..record_off, [0u8; 32]);
    put(&mut dsp, 4, 5);
    put(&mut dsp, 0x18, record_off as u32 + 32);
    put(&mut dsp, 0x3c, record_off as u32);
    for (i, w) in [2u32, 7, 0, 512, 0, 0, 0, 0].iter().enumerate() {
        dsp[record_off + i * 4..record_off + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(of(&dkm(4, 0, &[], &dsp)).unwrap().fx_slots, 1);
    for (word, value) in [
        (0, 1u32),
        (0, 3),
        (2, 2),
        (2, 520),
        (3, 4),
        (3, 520),
        (4, 1),
        (7, 1),
    ] {
        let mut bad = dsp.clone();
        bad[record_off + word * 4..record_off + word * 4 + 4].copy_from_slice(&value.to_le_bytes());
        assert_eq!(
            of(&dkm(4, 0, &[], &bad)),
            Err(Error::Dsp),
            "word {word}: {value}"
        );
    }
    let mut bad = dsp.clone();
    put(&mut bad, 0x3c, DSP_HEADER);
    assert_eq!(of(&dkm(4, 0, &[], &bad)), Err(Error::Dsp));
    let image_off = record_off + 32;
    dsp[image_off] = 1;
    assert_eq!(of(&dkm(4, 0, &[], &dsp)), Err(Error::Dsp));
}
