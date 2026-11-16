//! The upload commands that read or change the store: LIST, BEGIN, DATA, END, DELETE, MKDIR, RENAME, READ, VERIFY, CLEAR and ABORT.
//! Each handler decodes a request, calls the store steps of `store_writer.rs` and replies.

use crate::hash32::{self, Hash32};
use crate::store_writer::{self as sw, Bufs, IN_AT, SCRATCH_AT, TBL_CAP};
use crate::platform::{block_read, rd32, wr32};
use crate::card;
use core::ptr::addr_of_mut;
use dkrt_sdk::store::{self, Edit, Entry, Hdr, Table, KIND_DIR, KIND_FILE};
use dkrt_sdk::proto::*;
use dkrt_sdk::{CARD_TOO_SMALL, E_BUSY, OK};
use crate::transport::*;
use crate::transport::MAX_CHUNK;

pub(crate) const CLEAR_MAGIC: u32 = 0x434c_5221;

use crate::store_writer::{copy, io, lay, pe, sectors_of};

/// The store steps with the SysEx upload buffer `buf` (the table streams and the scratch are regions of it).
pub(crate) unsafe fn state(buf: *mut u8) -> Result<Table, i32> {
    sw::state(&Bufs::sysex(buf))
}

pub(crate) unsafe fn sync_headers(buf: *mut u8, t: &Table) -> bool {
    sw::sync_headers(&Bufs::sysex(buf), t)
}

pub(crate) unsafe fn do_commit(buf: *mut u8, cur: &Hdr, edit: Edit, pend: Option<(u32, u32)>) -> i32 {
    sw::do_commit(&Bufs::sysex(buf), cur, edit, pend)
}

/// The store paths a request carries: 1 to 159 bytes, NUL-terminated inside the body.
pub(crate) unsafe fn path_ok(p: *const u8, max: usize) -> Option<usize> {
    store::path_ok(p, max)
}

/// LIST: body `u32 first`, then optionally `prefix NUL` (1 to 159 bytes, e.g. `samples/`). Without a prefix: `first` is an entry index, `count` all
/// entries. With one: only the entries whose path starts with it (files, and folders with nothing in them), `first` indexes those and `count` is how many
/// there are. Reply: `u32 count`, `u8 n`, then per entry `path NUL, u32 size, hash[32]`, as many as fit, in table order (by first sector: a host sorts
/// for display). A folder with no file in it has size 0 and a zero hash slot.
pub(crate) unsafe fn cmd_list(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    if blen < 4 || (blen > 4 && (blen < 6 || blen > 4 + 160 || *body.add(blen - 1) != 0)) {
        return reply(CMD_LIST, seq, E_MSG, 0);
    }
    let first = rd32(body);
    let prefix = if blen > 4 { body.add(4) } else { core::ptr::null() };
    let mut plen = 0usize;
    if blen > 4 {
        plen = blen - 5;
        let mut i = 0;
        while i < plen {
            if *prefix.add(i) == 0 {
                return reply(CMD_LIST, seq, E_MSG, 0);
            }
            i += 1;
        }
    }
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_LIST, seq, e, 0),
    };
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    let mut at = 5usize;
    let mut n = 0u32;
    let mut seen = 0u32; // entries matching so far (prefix lists)
    let mut full = false;
    let from = if plen == 0 { first } else { 0 };
    let r = store::scan_from(&io(), &lay(), &t.h, from, buf.add(IN_AT), TBL_CAP, |_, e| {
        if plen != 0 {
            let mut i = 0;
            while i < plen {
                if *e.add(i) != *prefix.add(i) {
                    return true;
                }
                i += 1;
            }
            seen += 1;
            if seen <= first || full {
                return true;
            }
        }
        let pl = store::path_len(e);
        let need = pl + 1 + 4 + 32;
        if at + need > REPLY_BODY_MAX - 1 {
            full = true;
            return plen != 0; // a prefix list goes on to count the rest
        }
        let en = store::entry_of(e);
        copy(rb.add(at), e, pl);
        *rb.add(at + pl) = 0;
        wr32(rb.add(at + pl + 1), en.size);
        copy(rb.add(at + pl + 5), en.sha.as_ptr(), 32);
        at += need;
        n += 1;
        true
    });
    if r != OK {
        return reply(CMD_LIST, seq, pe(r), 0);
    }
    wr32(rb, if plen == 0 { t.h.count } else { seen });
    *rb.add(4) = n as u8;
    reply(CMD_LIST, seq, OK, at)
}

/// MKDIR: make the folder `path NUL` (a folder entry with nothing in it). Ok when it already exists (a folder entry, or implied by files inside it);
/// `E_PATH` when the path is a file or lies inside one. A folder entry is dropped when a file is stored inside the folder. An upload in progress is aborted.
pub(crate) unsafe fn cmd_mkdir(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    if !sw::preempt() {
        return reply(CMD_MKDIR, seq, E_BUSY, 0); // an SDK file is being written: its extent is not in the table yet
    }
    if !card::writable() {
        return reply(CMD_MKDIR, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    match path_ok(body, blen) {
        Some(n) if n + 1 == blen => {}
        _ => return reply(CMD_MKDIR, seq, if blen < 2 { E_MSG } else { E_PATH }, 0),
    }
    // pending files of an SDK module are named first, so that the checks below see them
    let r = sw::flush_pending(&Bufs::sysex(buf));
    if r != OK {
        return reply(CMD_MKDIR, seq, r, 0);
    }
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_MKDIR, seq, e, 0),
    };
    let mut exists = false;
    let mut conflict = false;
    let r = store::scan(&io(), &lay(), &t.h, buf.add(IN_AT), TBL_CAP, |_, e| {
        let rel = store::relation(body, e);
        if rel == 3 || (rel == 1 && store::entry_of(e).kind == KIND_FILE) {
            conflict = true;
        } else if rel == 1 || rel == 2 {
            exists = true;
        }
        !conflict
    });
    if r != OK {
        return reply(CMD_MKDIR, seq, pe(r), 0);
    }
    if conflict {
        return reply(CMD_MKDIR, seq, E_PATH, 0);
    }
    if exists {
        return reply(CMD_MKDIR, seq, OK, 0);
    }
    if !sync_headers(buf, &t) {
        return reply(CMD_MKDIR, seq, E_WRITE, 0);
    }
    let mut ent = [0u8; store::ENTRY];
    let e = Entry { first: 0, size: 0, kind: KIND_DIR, sha: [0; 32] };
    store::encode_entry(body, &e, ent.as_mut_ptr());
    let r = do_commit(buf, &t.h, Edit::Insert(ent.as_ptr()), None);
    reply(CMD_MKDIR, seq, r, 0)
}

/// RENAME: `from NUL to NUL`. Renames or moves one entry, a file or a folder with nothing in it, in one commit (`store::plan_rename`): the entry
/// keeps its data, size and sha256; a folder entry that `to` is inside goes; the folder `from` leaves gets a folder entry when nothing else is in it.
/// E_NOFILE: `from` is not an entry (a folder with files is not one); E_PATH: `to` is taken (a file, a folder, or inside a file), `from` itself, inside
/// `from`, or breaks the path rules. An upload in progress is aborted.
pub(crate) unsafe fn cmd_rename(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    if !sw::preempt() {
        return reply(CMD_RENAME, seq, E_BUSY, 0);
    }
    if !card::writable() {
        return reply(CMD_RENAME, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    let fl = match path_ok(body, blen) {
        Some(n) => n,
        None => return reply(CMD_RENAME, seq, if blen < 4 { E_MSG } else { E_PATH }, 0),
    };
    let to = body.add(fl + 1);
    match path_ok(to, blen - fl - 1) {
        Some(n) if fl + 1 + n + 1 == blen => {}
        _ => return reply(CMD_RENAME, seq, if blen < fl + 3 { E_MSG } else { E_PATH }, 0),
    }
    let r = sw::flush_pending(&Bufs::sysex(buf));
    if r != OK {
        return reply(CMD_RENAME, seq, r, 0);
    }
    let t = match state(buf) {
        Ok(t) => t,
        Err(e) => return reply(CMD_RENAME, seq, e, 0),
    };
    let plan = match store::plan_rename(&io(), &lay(), &t.h, buf.add(IN_AT), TBL_CAP, body, to) {
        Ok(p) => p,
        Err(e) => return reply(CMD_RENAME, seq, pe(e), 0),
    };
    if !sync_headers(buf, &t) {
        return reply(CMD_RENAME, seq, E_WRITE, 0);
    }
    let r = do_commit(buf, &t.h, plan.edit(body), None);
    reply(CMD_RENAME, seq, r, 0)
}

pub(crate) unsafe fn cmd_begin(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    if !sw::preempt() {
        return reply(CMD_BEGIN, seq, E_BUSY, 0);
    }
    if !card::writable() {
        return reply(CMD_BEGIN, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    let pl = match path_ok(body, blen) {
        Some(n) => n,
        None => return reply(CMD_BEGIN, seq, if blen < 1 + 4 + 32 { E_MSG } else { E_PATH }, 0),
    };
    if blen != pl + 1 + 4 + 32 {
        return reply(CMD_BEGIN, seq, E_MSG, 0);
    }
    let size = rd32(body.add(pl + 1));
    let mut sha = [0u8; 32];
    copy(sha.as_mut_ptr(), body.add(pl + 5), 32);
    let mut free = 0;
    let r = sw::begin(&Bufs::sysex(buf), sw::SYSEX, 0, body, pl, size, Some(sha), false, &mut free);
    if r == E_FULL {
        put32(1, free);
        return reply(CMD_BEGIN, seq, E_FULL, 4);
    }
    reply(CMD_BEGIN, seq, r, 0)
}

pub(crate) unsafe fn cmd_data(seq: u32, buf: *mut u8, blen: usize) -> i32 {
    if blen < 5 {
        // no upload open is the more telling status
        return reply(CMD_DATA, seq, if sw::sysex_open() { E_MSG } else { E_STATE }, 0);
    }
    let body = buf.add(BODY_AT);
    let len = (blen - 4) as u32;
    if len > MAX_CHUNK {
        return reply(CMD_DATA, seq, if sw::sysex_open() { E_LENGTH } else { E_STATE }, 0);
    }
    let r = sw::write(&Bufs::sysex(buf), sw::SYSEX, 0, 0, rd32(body), buf.add(BODY_AT + 4), len, true);
    reply(CMD_DATA, seq, r, 0)
}

/// The hash slot of `size` bytes of the card from store sector `first`, read in chunks through `buf`.
pub(crate) unsafe fn sha_of(buf: *mut u8, first: u32, size: u32, out: *mut u8) -> bool {
    let l = lay();
    let mut h = Hash32::new();
    let mut left = size;
    let mut sector = first;
    while left > 0 {
        let n = if left > MAX_CHUNK { MAX_CHUNK } else { left };
        let secs = sectors_of(n);
        if block_read(l.first + sector, secs * 512, buf.add(16)) != 0 {
            return false;
        }
        h.update(buf.add(16), n);
        left -= n;
        sector += secs;
    }
    core::ptr::copy_nonoverlapping(hash32::slot(h.finish()).as_ptr(), out, 32);
    true
}

pub(crate) unsafe fn cmd_end(seq: u32, buf: *mut u8) -> i32 {
    let r = sw::close(&Bufs::sysex(buf), sw::SYSEX, 0, 0, core::ptr::null_mut());
    reply(CMD_END, seq, r, 0)
}

/// DELETE: a path of the store (a file or a folder entry). The space is free again when the commit is done. An upload in progress is aborted.
pub(crate) unsafe fn cmd_delete(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    // an open SysEx upload is dropped by `sw::delete` only when the delete goes on to commit
    if sw::is_open() && !sw::sysex_open() {
        return reply(CMD_DELETE, seq, E_BUSY, 0);
    }
    if !card::writable() {
        return reply(CMD_DELETE, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    match path_ok(body, blen) {
        Some(n) if n + 1 == blen => {}
        _ => return reply(CMD_DELETE, seq, if blen < 2 { E_MSG } else { E_PATH }, 0),
    }
    let r = sw::delete(&Bufs::sysex(buf), body, true);
    reply(CMD_DELETE, seq, r, 0)
}

/// Most bytes one READ returns: the reply body is a status, the u32 file size and the bytes, in at most `REPLY_BODY_MAX`.
pub(crate) const READ_MAX: u32 = dkrt_sdk::proto::READ_MAX as u32;

/// The file `path` (NUL-terminated) of the store, or the status to reply.
pub(crate) unsafe fn file_of(buf: *mut u8, path: *const u8) -> Result<Entry, i32> {
    let t = state(buf)?;
    match store::find(&io(), &lay(), &t.h, buf.add(IN_AT), TBL_CAP, path) {
        Ok(Some((_, e))) if e.kind == KIND_FILE => Ok(e),
        Ok(_) => Err(E_NOFILE),
        Err(e) => Err(pe(e)),
    }
}

/// READ: path, NUL, u32 offset, u32 length. Reply: the file size (u32), then `min(length, 768, size - offset)` bytes.
/// E_PATH / E_MSG: bad request; E_NOFILE: not in the store; E_LENGTH: length 0; E_OFFSET: offset at or past the end of the file.
pub(crate) unsafe fn cmd_read(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    let pl = match path_ok(body, blen) {
        Some(n) if n + 1 + 8 == blen => n,
        _ => return reply(CMD_READ, seq, if blen < 10 { E_MSG } else { E_PATH }, 0),
    };
    let off = rd32(body.add(pl + 1));
    let mut len = rd32(body.add(pl + 5));
    if len == 0 {
        return reply(CMD_READ, seq, E_LENGTH, 0);
    }
    let f = match file_of(buf, body) {
        Ok(f) => f,
        Err(e) => return reply(CMD_READ, seq, e, 0),
    };
    if off >= f.size {
        return reply(CMD_READ, seq, E_OFFSET, 0);
    }
    if len > READ_MAX {
        len = READ_MAX;
    }
    if len > f.size - off {
        len = f.size - off;
    }
    // the sectors that hold the range, into the scratch region (3 sectors at most)
    let scratch = buf.add(SCRATCH_AT);
    let skip = off % 512;
    let secs = (skip + len + 511) / 512;
    if block_read(lay().first + f.first + off / 512, secs * 512, scratch) != 0 {
        return reply(CMD_READ, seq, E_READ, 0);
    }
    let rb = (addr_of_mut!(RBODY) as *mut u8).add(1);
    wr32(rb, f.size);
    copy(rb.add(4), scratch.add(skip as usize), len as usize);
    reply(CMD_READ, seq, OK, 4 + len as usize)
}

/// VERIFY: `path NUL`, optionally then `u32 offset, u32 length`. Reads the file (or that range of it) back from the card, in chunks, and replies its
/// hash slot (32 bytes): what a host compares with its own hash to know that the bytes on the card are the bytes it sent. The cost is about 7 ColdFire
/// instructions a byte, and a large file is still checked in ranges (the offset is a multiple of 512). Errors: E_NOFILE, E_READ, E_OFFSET (not a multiple of
/// 512, or at or past the end), E_LENGTH (0, or past the end of the file), E_MSG, E_PATH.
pub(crate) unsafe fn cmd_verify(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    let pl = match path_ok(body, blen) {
        Some(n) if n + 1 == blen || n + 1 + 8 == blen => n,
        _ => return reply(CMD_VERIFY, seq, if blen < 2 { E_MSG } else { E_PATH }, 0),
    };
    let f = match file_of(buf, body) {
        Ok(f) => f,
        Err(e) => return reply(CMD_VERIFY, seq, e, 0),
    };
    let (mut off, mut len) = (0u32, f.size);
    if blen > pl + 1 {
        off = rd32(body.add(pl + 1));
        len = rd32(body.add(pl + 5));
        if off & 511 != 0 || off >= f.size {
            return reply(CMD_VERIFY, seq, E_OFFSET, 0);
        }
        if len == 0 || len as u64 > (f.size - off) as u64 {
            return reply(CMD_VERIFY, seq, E_LENGTH, 0);
        }
    }
    if !sha_of(buf, f.first + off / 512, len, (addr_of_mut!(RBODY) as *mut u8).add(1)) {
        return reply(CMD_VERIFY, seq, E_READ, 0);
    }
    reply(CMD_VERIFY, seq, OK, 32)
}

pub(crate) unsafe fn cmd_clear(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    // CLEAR frees every extent: a reader that still reads one (a sample load) keeps the store as it is
    if sw::leased_any() {
        return reply(CMD_CLEAR, seq, E_FILE_IN_USE, 0);
    }
    if !sw::preempt() {
        return reply(CMD_CLEAR, seq, E_BUSY, 0);
    }
    if !card::writable() {
        return reply(CMD_CLEAR, seq, if card::state() == CARD_TOO_SMALL { E_NOSTORE } else { E_INUSE }, 0);
    }
    if blen != 4 || rd32(body) != CLEAR_MAGIC {
        return reply(CMD_CLEAR, seq, E_MSG, 0);
    }
    // CLEAR works on a damaged store too: the new sequence number is above every copy that is readable
    let seq_now = match state(buf) {
        Ok(t) => t.h.seq,
        Err(_) => highest_seq(buf),
    };
    let cur = Hdr { seq: seq_now, ..Hdr::empty(&lay()) };
    let r = do_commit(buf, &cur, Edit::Clear, None);
    if r == OK {
        sw::pend_clear(); // the store is emptied: pending files go too (only once it is)
    }
    reply(CMD_CLEAR, seq, r, 0)
}

/// The higher `seq` of the two header copies that parse (0 when none).
pub(crate) unsafe fn highest_seq(buf: *mut u8) -> u32 {
    let l = lay();
    let mut best = 0;
    let mut k = 0;
    while k < 2 {
        let rel = if k == 0 { 0 } else { l.header_b() };
        if block_read(l.first + rel, 512, buf.add(SCRATCH_AT)) == 0 {
            if let Some(h) = store::parse_header(buf.add(SCRATCH_AT)) {
                if h.seq > best {
                    best = h.seq;
                }
            }
        }
        k += 1;
    }
    best
}

pub(crate) unsafe fn h_list(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_list(seq, buf, body, blen)
}
pub(crate) unsafe fn h_begin(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_begin(seq, buf, body, blen)
}
pub(crate) unsafe fn h_data(seq: u32, buf: *mut u8, _body: *const u8, blen: usize) -> i32 {
    cmd_data(seq, buf, blen)
}
pub(crate) unsafe fn h_end(seq: u32, buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    cmd_end(seq, buf)
}
pub(crate) unsafe fn h_mkdir(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_mkdir(seq, buf, body, blen)
}
pub(crate) unsafe fn h_rename(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_rename(seq, buf, body, blen)
}
pub(crate) unsafe fn h_delete(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_delete(seq, buf, body, blen)
}
pub(crate) unsafe fn h_read(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_read(seq, buf, body, blen)
}
pub(crate) unsafe fn h_verify(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_verify(seq, buf, body, blen)
}
pub(crate) unsafe fn h_clear(seq: u32, buf: *mut u8, body: *const u8, blen: usize) -> i32 {
    cmd_clear(seq, buf, body, blen)
}
pub(crate) unsafe fn h_abort(seq: u32, _buf: *mut u8, _body: *const u8, _blen: usize) -> i32 {
    sw::abort_sysex();
    reply(CMD_ABORT, seq, OK, 0)
}
