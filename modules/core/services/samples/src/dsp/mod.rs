//! Sample part of the DSP loader: the track-table record of a DSP section.
use crate::tracks;
use dkrt_abi::dsp::{in_sec, rd32le, DspSec, Job, FX_REC};
use dkrt_abi::mem::rd32;
use dkrt_abi::service::Claim;
use dkrt_sdk::*;

/// DSP section, version 5: the track-table record (`bank_off` is the record's offset in the section, validated here).
pub(crate) unsafe fn check_sections(sec: &DspSec) -> i32 {
    if sec.version != 5 {
        return OK;
    }
    let DspSec {
        s,
        size,
        image_off,
        image_size,
        reloc_off,
        nreloc,
        import_off,
        nimport,
        voice_off,
        nvoice,
        vstride,
        fx_off,
        nfx,
        bank_off,
        ..
    } = *sec;
    if bank_off < 64 || bank_off & 3 != 0 || !in_sec(bank_off, 32, size) {
        return E_DSP_FORMAT;
    }
    // The record must not alias another section table or the initialized image.
    for (o, n) in [
        (image_off, image_size),
        (reloc_off, nreloc * 8),
        (import_off, nimport * 32),
        (voice_off, nvoice * vstride),
        (fx_off, nfx * FX_REC),
    ] {
        if n != 0 && bank_off < o + n && o < bank_off + 32 {
            return E_DSP_FORMAT;
        }
    }
    let regions = [
        (image_off, image_size),
        (reloc_off, nreloc * 8),
        (import_off, nimport * 32),
        (voice_off, nvoice * vstride),
        (fx_off, nfx * FX_REC),
    ];
    for (i, &(o, n)) in regions.iter().enumerate() {
        if n != 0 && o < 64 {
            return E_DSP_FORMAT;
        }
        for &(p, m) in &regions[..i] {
            if n != 0 && m != 0 && o < p + m && p < o + n {
                return E_DSP_FORMAT;
            }
        }
    }
    let b = s.add(bank_off as usize);
    let record = core::slice::from_raw_parts(b, 32);
    let image = core::slice::from_raw_parts(s.add(image_off as usize), image_size as usize);
    // The record is a per-track table (kind 2) with its two blocks.
    if rd32le(b) != 2 || !dkrt_sdk::sample_job::valid_tracks(record, image) {
        return E_DSP_FORMAT;
    }
    // Relocations cannot turn the initially empty table into a published one.
    let (records, control) = (rd32le(b.add(8)), rd32le(b.add(12)));
    for k in 0..nreloc {
        let at = rd32(s.add((reloc_off + k * 8) as usize));
        if (at < records + dkrt_sdk::sample_job::TRACK_RECORDS_BYTES
            && records < at.saturating_add(6))
            || (at < control + dkrt_sdk::sample_job::TRACK_CONTROL_BYTES
                && control < at.saturating_add(6))
        {
            return E_DSP_FORMAT;
        }
    }
    OK
}

/// A track table needs a free client slot: refuse before anything is reserved.
pub(crate) unsafe fn admit(job: &Job) -> i32 {
    if !job.bank.is_null() && rd32le(job.bank) == 2 && !tracks::can_reserve() {
        return E_TABLE_FULL;
    }
    OK
}

/// Bind the module's track table to a client slot (`admit` checked one free).
pub(crate) unsafe fn claim(c: &mut Claim) {
    let (rec, job, base) = (c.rec, c.job, c.base);
    if !job.bank.is_null() {
        tracks::reserve(
            rec,
            rd32le(job.bank.add(4)),
            base + rd32le(job.bank.add(8)),
            base + rd32le(job.bank.add(12)),
        );
    }
}
