//! The `.dkm` image parse: the header, bounds and relocation checks of the loader, on the staged bytes only. Nothing is written.
//! The loader calls `head` first, then its import check, then `sections`. Each returns the error code the loader returns.

use crate::sha256::Sha;
use crate::{cstr_len, rd32};
use dkrt_sdk::*;

const MAGIC: u32 = dkneeds::MAGIC;
const HEADER: u32 = dkneeds::HEADER;
const NONE: u32 = dkneeds::NONE;

/// The header fields the loader uses after the checks (all validated by `head`).
#[derive(Clone, Copy, Debug)]
pub struct Head {
    /// The runtime version the module needs (header word, with the trace flag bit).
    pub needs: u32,
    pub image_off: u32,
    pub image_size: u32,
    pub bss: u32,
    pub reloc_off: u32,
    pub reloc_n: u32,
    pub import_off: u32,
    pub import_n: u32,
    pub init: u32,
    pub fini: u32,
    pub menu: u32,
    pub tick: u32,
}

pub unsafe fn in_file(off: u32, len: u32, size: u32) -> bool {
    off <= size && len <= size - off
}

/// Magic, format, sizes, sha256, runtime version, name, then the field and bounds checks, in that order. `f` points at `size` staged bytes.
pub unsafe fn head(f: *const u8, size: u32) -> Result<Head, i32> {
    if size < HEADER || rd32(f) != MAGIC || rd32(f.add(4)) != 1 {
        return Err(E_FORMAT);
    }
    let hdr = rd32(f.add(dkneeds::H_HDRSIZE as usize));
    if rd32(f.add(dkneeds::H_SIZE as usize)) != size || hdr != HEADER {
        return Err(E_FORMAT);
    }
    // the file's own sha256, over the file with the sha field read as zeros
    let h_sha = dkneeds::H_SHA as usize;
    let mut want = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        want[i] = *f.add(h_sha + i);
        i += 1;
    }
    let mut sha = Sha::new();
    sha.update(f, h_sha as u32);
    sha.update_zeros(32);
    sha.update(f.add(h_sha + 32), size - (h_sha as u32 + 32));
    let mut got = [0u8; 32];
    sha.finish(got.as_mut_ptr());
    if got != want {
        return Err(E_SHA);
    }
    // the runtime version the module needs: another major or minor than the runtime's, or a higher patch, refuses (0.x: a minor breaks)
    let needs = rd32(f.add(dkneeds::H_RUNTIME as usize));
    if !crate::runtime_accepts(needs) {
        return Err(E_CONTRACT);
    }
    let name = f.add(dkneeds::H_NAME as usize);
    let nlen = cstr_len(name, dkneeds::NAME_LEN as usize);
    if nlen == 0 || nlen == dkneeds::NAME_LEN as usize {
        return Err(E_FORMAT);
    }
    let image_off = rd32(f.add(dkneeds::H_IMAGE_OFF as usize));
    let image_size = rd32(f.add(dkneeds::H_IMAGE_SIZE as usize));
    let bss = rd32(f.add(dkneeds::H_BSS as usize));
    let align = rd32(f.add(dkneeds::H_ALIGN as usize));
    let reloc_off = rd32(f.add(dkneeds::H_RELOC_OFF as usize));
    let reloc_n = rd32(f.add(dkneeds::H_RELOC_N as usize));
    let import_off = rd32(f.add(dkneeds::H_IMPORT_OFF as usize));
    let import_n = rd32(f.add(dkneeds::H_IMPORT_N as usize));
    let init = rd32(f.add(dkneeds::H_INIT as usize));
    let fini = rd32(f.add(dkneeds::H_FINI as usize));
    let menu = rd32(f.add(dkneeds::H_MENU as usize));
    let tick = rd32(f.add(dkneeds::H_TICK as usize));
    if align == 0
        || align > 16
        || align & (align - 1) != 0
        || image_size == 0
        || image_size > size
        || bss > crate::ARENA_BYTES as u32
    {
        return Err(E_FORMAT);
    }
    if !in_file(image_off, image_size, size)
        || image_off & 15 != 0
        || reloc_n > dkneeds::MAX_RELOCS
        || import_n > dkneeds::MAX_CF_IMPORTS
        || !in_file(reloc_off, reloc_n * dkneeds::CF_RELOC_LEN, size)
        || !in_file(import_off, import_n * dkneeds::CF_IMPORT_LEN, size)
    {
        return Err(E_FORMAT);
    }
    if (init != NONE && init >= image_size)
        || (fini != NONE && fini >= image_size)
        || (menu != NONE && menu >= image_size)
        || (tick != NONE && tick >= image_size)
    {
        return Err(E_FORMAT);
    }
    Ok(Head { needs, image_off, image_size, bss, reloc_off, reloc_n, import_off, import_n, init, fini, menu, tick })
}

/// The relocation table: kind, bounds, import index. Runs after the loader's import check.
pub unsafe fn sections(f: *const u8, head: &Head) -> Result<(), i32> {
    let mut i = 0;
    while i < head.reloc_n as usize {
        let e = f.add(head.reloc_off as usize + i * dkneeds::CF_RELOC_LEN as usize);
        let off = rd32(e.add(dkneeds::CF_RELOC_OFFSET as usize));
        let kind = rd32(e.add(dkneeds::CF_RELOC_KIND as usize));
        let k = kind >> 24;
        if off > head.image_size - 4 || (k != 0 && k != 1) || (k == 1 && (kind & 0xff_ffff) >= head.import_n) {
            return Err(E_FORMAT);
        }
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE_OFF: usize = 160;
    const RELOC_OFF: usize = 176;
    const IMPORT_OFF: usize = 192;
    const LEN: usize = 224;

    fn put(v: &mut [u8], off: u32, val: u32) {
        v[off as usize..off as usize + 4].copy_from_slice(&val.to_be_bytes());
    }

    /// Recompute the size field and the sha256 after a change.
    fn seal(v: &mut [u8]) {
        let n = v.len() as u32;
        put(v, dkneeds::H_SIZE, n);
        let mut sha = Sha::new();
        let mut got = [0u8; 32];
        unsafe {
            sha.update(v.as_ptr(), dkneeds::H_SHA);
            sha.update_zeros(32);
            sha.update(v.as_ptr().add(dkneeds::H_SHA as usize + 32), n - (dkneeds::H_SHA + 32));
            sha.finish(got.as_mut_ptr());
        }
        v[dkneeds::H_SHA as usize..dkneeds::H_SHA as usize + 32].copy_from_slice(&got);
    }

    /// One image: 16 bytes at 160, two relocations at 176 (image-relative, import 0), one import slot at 192.
    fn good() -> Vec<u8> {
        let mut v = vec![0u8; LEN];
        put(&mut v, dkneeds::H_MAGIC, MAGIC);
        put(&mut v, dkneeds::H_FORMAT, 1);
        put(&mut v, dkneeds::H_HDRSIZE, HEADER);
        v[dkneeds::H_NAME as usize] = b'm';
        put(&mut v, dkneeds::H_IMAGE_OFF, IMAGE_OFF as u32);
        put(&mut v, dkneeds::H_IMAGE_SIZE, 16);
        put(&mut v, dkneeds::H_ALIGN, 4);
        put(&mut v, dkneeds::H_RELOC_OFF, RELOC_OFF as u32);
        put(&mut v, dkneeds::H_RELOC_N, 2);
        put(&mut v, dkneeds::H_IMPORT_OFF, IMPORT_OFF as u32);
        put(&mut v, dkneeds::H_IMPORT_N, 1);
        put(&mut v, dkneeds::H_INIT, NONE);
        put(&mut v, dkneeds::H_FINI, NONE);
        put(&mut v, dkneeds::H_MENU, NONE);
        put(&mut v, dkneeds::H_TICK, NONE);
        put(&mut v, RELOC_OFF as u32, 0);
        put(&mut v, RELOC_OFF as u32 + 4, 0);
        put(&mut v, RELOC_OFF as u32 + 8, 12);
        put(&mut v, RELOC_OFF as u32 + 12, 1 << 24);
        seal(&mut v);
        v
    }

    fn run(v: &[u8]) -> Result<Head, i32> {
        unsafe { head(v.as_ptr(), v.len() as u32) }
    }

    /// Change one header word, seal, and expect `code` from `head`.
    fn refuses(field: u32, val: u32, code: i32) {
        let mut v = good();
        put(&mut v, field, val);
        seal(&mut v);
        assert_eq!(run(&v).unwrap_err(), code, "field {field} = {val}");
    }

    #[test]
    fn good_header_parses() {
        let v = good();
        let h = run(&v).unwrap();
        assert_eq!((h.image_off, h.image_size, h.bss), (160, 16, 0));
        assert_eq!((h.reloc_off, h.reloc_n, h.import_off, h.import_n), (176, 2, 192, 1));
        assert_eq!((h.init, h.fini, h.menu, h.tick), (NONE, NONE, NONE, NONE));
        assert_eq!(unsafe { sections(v.as_ptr(), &h) }, Ok(()));
    }

    #[test]
    fn refuses_bad_magic_format_and_sizes() {
        let v = good();
        assert_eq!(run(&v[..100]).unwrap_err(), E_FORMAT);
        refuses(dkneeds::H_MAGIC, 0, E_FORMAT);
        refuses(dkneeds::H_FORMAT, 2, E_FORMAT);
        refuses(dkneeds::H_HDRSIZE, HEADER + 4, E_FORMAT);
        let mut v = good();
        put(&mut v, dkneeds::H_SIZE, LEN as u32 + 1);
        assert_eq!(run(&v).unwrap_err(), E_FORMAT);
    }

    #[test]
    fn refuses_bad_sha_and_contract() {
        let mut v = good();
        v[IMAGE_OFF] ^= 1;
        assert_eq!(run(&v).unwrap_err(), E_SHA);
        refuses(dkneeds::H_RUNTIME, 0x7fff_ff00, E_CONTRACT);
    }

    #[test]
    fn refuses_bad_name() {
        let mut v = good();
        v[dkneeds::H_NAME as usize] = 0;
        seal(&mut v);
        assert_eq!(run(&v).unwrap_err(), E_FORMAT);
        let mut v = good();
        let n = dkneeds::H_NAME as usize;
        v[n..n + dkneeds::NAME_LEN as usize].fill(b'a');
        seal(&mut v);
        assert_eq!(run(&v).unwrap_err(), E_FORMAT);
    }

    #[test]
    fn refuses_bad_fields() {
        refuses(dkneeds::H_ALIGN, 0, E_FORMAT);
        refuses(dkneeds::H_ALIGN, 32, E_FORMAT);
        refuses(dkneeds::H_ALIGN, 3, E_FORMAT);
        refuses(dkneeds::H_IMAGE_SIZE, 0, E_FORMAT);
        refuses(dkneeds::H_IMAGE_SIZE, LEN as u32 + 1, E_FORMAT);
        refuses(dkneeds::H_BSS, crate::ARENA_BYTES as u32 + 1, E_FORMAT);
    }

    #[test]
    fn refuses_out_of_bounds() {
        refuses(dkneeds::H_IMAGE_OFF, LEN as u32 - 8, E_FORMAT);
        refuses(dkneeds::H_IMAGE_OFF, IMAGE_OFF as u32 + 8, E_FORMAT);
        refuses(dkneeds::H_RELOC_N, dkneeds::MAX_RELOCS + 1, E_FORMAT);
        refuses(dkneeds::H_IMPORT_N, dkneeds::MAX_CF_IMPORTS + 1, E_FORMAT);
        refuses(dkneeds::H_RELOC_OFF, LEN as u32 - 8, E_FORMAT);
        refuses(dkneeds::H_IMPORT_OFF, LEN as u32 - 8, E_FORMAT);
    }

    #[test]
    fn refuses_entry_points_outside_the_image() {
        refuses(dkneeds::H_INIT, 16, E_FORMAT);
        refuses(dkneeds::H_FINI, 16, E_FORMAT);
        refuses(dkneeds::H_MENU, 16, E_FORMAT);
        refuses(dkneeds::H_TICK, 16, E_FORMAT);
    }

    fn sections_of(edit: impl Fn(&mut [u8])) -> Result<(), i32> {
        let mut v = good();
        edit(&mut v);
        seal(&mut v);
        let h = run(&v).unwrap();
        unsafe { sections(v.as_ptr(), &h) }
    }

    #[test]
    fn refuses_bad_relocations() {
        // offset past the last word of the image
        assert_eq!(sections_of(|v| put(v, RELOC_OFF as u32, 13)), Err(E_FORMAT));
        // unknown kind
        assert_eq!(sections_of(|v| put(v, RELOC_OFF as u32 + 4, 2 << 24)), Err(E_FORMAT));
        // import index not below the import count
        assert_eq!(sections_of(|v| put(v, RELOC_OFF as u32 + 12, (1 << 24) | 1)), Err(E_FORMAT));
        // the last valid offset and import index
        assert_eq!(sections_of(|v| put(v, RELOC_OFF as u32, 12)), Ok(()));
    }
}
