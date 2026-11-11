/* core/plusdrive-write on the Digitakt II 1.17: file writes to the stock +Drive through the firmware's own C-level file code (finding 120).
 * The plug-in point is runtime/include/dkrt_pdw.h; the session rules (owner, handle, offsets, expiry) are the runtime's.
 *
 * A file is written under the staging folder /.dkstage, complete with the 16 zero bytes the firmware stores after every file and its close hash,
 * and moved into place with the firmware's rename (the inode and its hash stay). A cut at any point leaves a half-written file only in the
 * staging folder; the boot recovery removes it. A replace is: write a hashed journal record naming the destination, delete the destination,
 * rename the staged file, delete the journal. The recovery finishes a replace whose destination is missing and drops one whose destination is
 * still there. The folder exists only while something is staged: it is removed whenever it is empty.
 *
 * Facts the order of steps relies on (finding 120, static, firmware 1.17):
 *  - Every inode, directory page and bitmap update is written through to the eMMC by the firmware's block write before the file call returns
 *    (FUN_4015b868, FUN_40155c34, FUN_4015bb7e call the block write, which waits for the transfer), in call order. No flush is needed between steps.
 *  - An inode has a link count (+2). Adding an entry (rename, link) writes the incremented count first; deleting a name removes the entry, then
 *    decrements the count, and frees the data only when it reaches 0. So one name of a file with two names can be deleted without freeing the
 *    data, and a cut inside a rename leaves two names with count 2.
 *  - Names are compared byte for byte (case-sensitive).
 *
 * The filesystem mutex of the firmware protects every call; this adapter keeps one set of state and the runtime makes one call at a time. */

#include <stddef.h>
#include <stdint.h>
#include "dkrt_pdw.h"
#include "dk_fw_storage.h"

typedef uint32_t u32;
typedef int32_t i32;
typedef uint8_t u8;

#define E_EXISTS (-18)
#define E_PATH (-51)
#define E_FULL (-52)
#define E_LENGTH (-55)
#define E_HASH (-56)
#define E_WRITE (-57)
#define E_NOFILE (-59)
#define E_FILE_IN_USE (-71)

#define STAGE_DIR "/.dkstage"
#define DEST_MAX 256u
#define NAMES 24u
#define ZERO_TAIL 16u
/* a staged name: 's' and 8 hex digits, optionally followed by ".dest" (the journal) */
#define STAGE_LEN 9u
#define JOUR_LEN 14u

/* What the firmware writes (it runs outside the patch code, so the write-watch of the proofs leaves this struct out: DKPW_DMA, 640 bytes).
 * Every buffer handed to the firmware lives here, 16-byte aligned. */
struct dma {
    u8 jbuf[256];        /* journal read */
    u32 hnd[4];          /* the staged file's handle */
    u32 jh[4];           /* the journal's handle */
    u32 rh[4];           /* a read handle */
    u32 inf[4], inf2[4]; /* file_info results */
    u32 dstate[4];       /* directory state */
    u32 dout[0x114 / 4]; /* one directory entry */
    u8 pad[640 - 256 - 96 - 0x114];
};
struct dma DKPW_DMA __attribute__((aligned(16)));
_Static_assert(sizeof(struct dma) == 640, "DKPW_DMA is 640 bytes (tests/regression/runtime/write-safety.ts)");
_Static_assert(offsetof(struct dma, jbuf) % 16 == 0 && offsetof(struct dma, hnd) % 16 == 0 && offsetof(struct dma, jh) % 16 == 0 &&
                   offsetof(struct dma, rh) % 16 == 0 && offsetof(struct dma, inf) % 16 == 0 && offsetof(struct dma, inf2) % 16 == 0 &&
                   offsetof(struct dma, dstate) % 16 == 0 && offsetof(struct dma, dout) % 16 == 0,
               "buffers handed to the firmware are 16-byte aligned");
#define hnd (DKPW_DMA.hnd)
#define jh (DKPW_DMA.jh)
#define rh (DKPW_DMA.rh)
#define inf (DKPW_DMA.inf)
#define dstate (DKPW_DMA.dstate)
#define dout (DKPW_DMA.dout)
#define jbuf (DKPW_DMA.jbuf)

static u8 st_open;   /* `hnd` is an open staged file */
static u8 st_live;   /* a staged file exists and the caller still owns it */
static u8 broken;    /* a write failed: nothing more is written to this file */
static u8 have_want, replace;
static u8 want[4];
/* Test knob (bits 8 to 15 of the flags of dkpw_begin, set by the runtime's DKRT_PD_CUT): the adapter stops dead at this step of the write, as a
 * power cut would: 1 after the first data write, 2 after the hash and close, 3 after the journal, 4 after the destination is deleted, 5 after
 * the rename, 6 between the two halves of the rename (a second name added, the staged name still there). From then on every call answers E_WRITE
 * and touches nothing, so that the card is the state a cut leaves. 7 is no cut: the rename fails (as the firmware could refuse it) and the
 * recovery must finish the replace. 8 leaves an unhashed file at the destination (a stock writer that died): no hash, rename, dead. */
static u8 cut, dead;
static u32 total, written;
static char stage[40], jour[48];
static char dest[DEST_MAX] __attribute__((aligned(16)));
static char lp[DEST_MAX];
static const u8 zeros[ZERO_TAIL] __attribute__((aligned(16)));

#define PDW_TRACE 0 /* 1 for hardware measurement builds: per-step times in the log at close */
#if PDW_TRACE
/* the runtime (dkrt.h) */
extern u32 dkrt_blocks(void); /* the 1500 Hz block counter: 1 unit = 0.667 ms */
extern void dkrt_log_ev(u32 kind, u32 flags, const u32 *a);
/* sdk.toml [[log.kind]] 14 MOD_LOG: args msg0..msg3 (16 ASCII chars, big-endian words) and value; flags: not an error, level 2 (info) */
#define PW_KIND_MOD_LOG 14u
#define PW_FLAGS (2u << 1)
/* Times in blocks, summed over the calls of one file; a short call often reads 0, so only the sums mean something. */
static u32 pw_b_parent, pw_b_lookup, pw_b_mkdir, pw_b_open, pw_b_reserve;
static u32 pw_e_tail, pw_e_hash, pw_e_close, pw_e_info, pw_e_place, pw_e_rename, pw_e_journal;
static u32 pw_w_blk, pw_w_n;
static u32 pw_t;
#define PW_START() (pw_t = dkrt_blocks())
#define PW_STOP(c) ((c) += dkrt_blocks() - pw_t)

/* `tag`: a string of at most 16 chars, padded with NULs */
static void pw_log(const char *tag, u32 value)
{
    u32 a[5];
    u32 i;
    u32 live = 1;
    for (i = 0; i < 16u; i++) {
        u32 c = 0;
        if (live && tag[i] != 0)
            c = (u8)tag[i];
        else
            live = 0;
        if ((i & 3u) == 0u)
            a[i >> 2] = 0;
        a[i >> 2] |= c << (24u - 8u * (i & 3u));
    }
    a[4] = value;
    dkrt_log_ev(PW_KIND_MOD_LOG, PW_FLAGS, a);
}

static void pw_emit(void)
{
    pw_log("pw.b.parent", pw_b_parent);
    pw_log("pw.b.lookup", pw_b_lookup);
    pw_log("pw.b.mkdir", pw_b_mkdir);
    pw_log("pw.b.open", pw_b_open);
    pw_log("pw.b.reserve", pw_b_reserve);
    pw_log("pw.e.tail", pw_e_tail);
    pw_log("pw.e.hash", pw_e_hash);
    pw_log("pw.e.close", pw_e_close);
    pw_log("pw.e.info", pw_e_info);
    pw_log("pw.e.place", pw_e_place);
    pw_log("pw.e.rename", pw_e_rename);
    pw_log("pw.e.journal", pw_e_journal);
    pw_log("pw.w.blk", pw_w_blk);
    pw_log("pw.w.n", pw_w_n);
}
static u32 pw_t2; /* the clock of the steps inside `place` (pw_t times place as a whole) */
#define PW_T0() PW_START()
#define PW_T1(c) PW_STOP(c)
#define PW_I0() (pw_t2 = dkrt_blocks())
#define PW_I1(c) ((c) += dkrt_blocks() - pw_t2)
#else
#define PW_T0() ((void)0)
#define PW_T1(c) ((void)0)
#define PW_I0() ((void)0)
#define PW_I1(c) ((void)0)
#define pw_emit() ((void)0)
#endif

static int cut_at(u8 step)
{
    if (cut == step) dead = 1;
    return dead;
}

u32 dkpw_version(void) { return DKPW_VERSION; }
const u8 *dkpw_staging(void) { return (const u8 *)STAGE_DIR; }

static u32 slen(const char *s)
{
    u32 n = 0;
    while (s[n]) n++;
    return n;
}

static void scopy(char *d, const char *s)
{
    u32 i = 0;
    while ((d[i] = s[i]) != 0) i++;
}

static int seq(const char *a, const char *b)
{
    u32 i = 0;
    while (a[i] == b[i]) {
        if (!a[i]) return 1;
        i++;
    }
    return 0;
}

/* The value of a 4-byte address literal that a generated site names (the site is one byte as far as the compiler knows). */
static u32 lit(const u8 *site)
{
    __asm__("" : "+r"(site));
    return *(const volatile u32 *)site;
}

static i32 f_delete(const char *p) { return dk_fw_storage_file_delete((const u8 *)p); }
static i32 f_info(const char *p, u32 *out) { return dk_fw_storage_file_info((const u8 *)p, out); }
static i32 f_mkdir(const char *p) { return dk_fw_storage_file_mkdir((const u8 *)p); }

/* The firmware's mounted flag: every path function of the filesystem starts by testing it. */
static int mounted(void) { return *(const volatile u32 *)lit(&dk_fw_storage_mounted) != 0; }

/* ---- directory scans ---------------------------------------------------------------------------------------------------------- */

#define ENT_NAME ((const char *)dout + 20)
#define ENT_ID (dout[0])
#define ENT_FOLDER (*(const volatile uint16_t *)((const u8 *)dout + 4) == 1)

/* Open directory `p`; 1 when it is there. */
static int dir_open(const char *p) { return dk_fw_storage_directory_open((const u8 *)p, dstate) == 0; }

/* The next entry in `dout` (never "." or ".."); 0 at the end. */
static int dir_next(void)
{
    u32 i;
    for (;;) {
        const char *nm;
        for (i = 0; i < 0x114 / 4; i++) dout[i] = 0;
        if (dk_fw_storage_directory_next(dstate, (u8 *)dout) != 1) return 0;
        nm = ENT_NAME;
        if (nm[0] == '.' && (nm[1] == 0 || (nm[1] == '.' && nm[2] == 0))) continue;
        return 1;
    }
}

/* What is at the absolute path `p`: 0 nothing (or the parent is not there), 1 a file (hashed or not), 2 a folder; `id` gets its inode id.
 * file_info sees neither folders nor unhashed files, so this scans the parent folder. */
static int lookup(const char *p, u32 *id)
{
    u32 n = slen(p), last = 0, i;
    if (n >= DEST_MAX) return 0;
    for (i = 0; i < n; i++)
        if (p[i] == '/') last = i;
    scopy(lp, p);
    if (last == 0) scopy(lp, "/");
    else lp[last] = 0;
    if (!dir_open(lp)) return 0;
    while (dir_next()) {
        if (seq(ENT_NAME, p + last + 1)) {
            *id = ENT_ID;
            return ENT_FOLDER ? 2 : 1;
        }
    }
    return 0;
}

/* Create every missing folder above the file `p` (a path with a leading '/'). The errors of existing folders are ignored. */
static void parents(char *p)
{
    u32 i;
    for (i = 1; p[i]; i++) {
        if (p[i] == '/') {
            p[i] = 0;
            f_mkdir(p);
            p[i] = '/';
        }
    }
}

/* Is a part of the path above the file `p` a file? */
static int parent_is_file(char *p)
{
    u32 i, id;
    int r = 0;
    for (i = 1; p[i] && !r; i++) {
        if (p[i] == '/') {
            p[i] = 0;
            r = lookup(p, &id) == 1;
            p[i] = '/';
        }
    }
    return r;
}

static int hexv(char c) { return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'); }

/* Is `s` one of our names: "s" and 8 hex digits, or that and ".dest" (`is_journal`)? */
static int ours(const char *s, int *is_journal)
{
    u32 i, n = slen(s);
    if (n != STAGE_LEN && n != JOUR_LEN) return 0;
    if (s[0] != 's') return 0;
    for (i = 1; i < STAGE_LEN; i++)
        if (!hexv(s[i])) return 0;
    *is_journal = n == JOUR_LEN;
    return n == STAGE_LEN || seq(s + STAGE_LEN, ".dest");
}

static u32 hexval(const char *s)
{
    u32 v = 0, i;
    for (i = 1; i < STAGE_LEN; i++) v = (v << 4) | (u32)(s[i] <= '9' ? s[i] - '0' : s[i] - 'a' + 10);
    return v;
}

static void names_for(u32 n)
{
    static const char hex[] = "0123456789abcdef";
    u32 i;
    scopy(stage, STAGE_DIR "/s");
    for (i = 0; i < 8; i++) stage[11 + i] = hex[(n >> (28 - 4 * i)) & 15];
    stage[19] = 0;
    scopy(jour, stage);
    scopy(jour + 19, ".dest");
}

/* The staging folder is empty (or not there)? */
static int staging_empty(void)
{
    if (!dir_open(STAGE_DIR)) return 1;
    return !dir_next();
}

/* Remove the folder when it is empty. Only the boot recovery and the runtime's idle call (`dkpw_recover`) do it: the folder lives for a whole
 * upload session, because its creation (about 420 ms) and removal (about 150 ms) cost more than a small file. */
static void tidy(void)
{
    if (!dead && staging_empty()) dk_fw_storage_file_rmdir((const u8 *)STAGE_DIR);
}

/* The folder /factory (and what is under it) is the firmware's own read-only ROM tree: its inodes and directory pages are RAM tables built at every
 * mount (ids from 0x1000000), never written to the card. Nothing is written, replaced or deleted there. */
static int in_factory(const char *p)
{
    static const char f[] = "factory";
    u32 i;
    for (i = 0; f[i]; i++)
        if (p[i] != f[i]) return 0;
    return p[i] == 0 || p[i] == '/';
}

/* ---- write ------------------------------------------------------------------------------------------------------------------- */

void dkpw_abort(void)
{
    if (dead) return;
    if (st_open) {
        dk_fw_storage_path_close(hnd);
        st_open = 0;
    }
    if (st_live) {
        f_delete(stage);
        st_live = 0;
    }
}

/* A number above every one of our names in the folder: a leftover (which only the recovery removes) is never reused. `there` gets 1 when the
 * folder exists. */
static u32 next_number(int *there)
{
    u32 top = 0;
    int any = 0, j;
    *there = dir_open(STAGE_DIR);
    if (*there) {
        while (dir_next()) {
            if (ours(ENT_NAME, &j)) {
                u32 v = hexval(ENT_NAME);
                if (!any || v > top) top = v;
                any = 1;
            }
        }
    }
    return any ? top + 1 : 0;
}

int32_t dkpw_begin(const u8 *path, u32 size, const u8 *hash4, u32 flags)
{
    u32 n = slen((const char *)path), i, id;
    int at, there;
    u32 num;
    if (n == 0 || n + 1 >= DEST_MAX || in_factory((const char *)path)) return E_PATH;
    if (st_live || st_open) dkpw_abort();
    dest[0] = '/';
    for (i = 0; i <= n; i++) dest[1 + i] = (char)path[i];
    replace = (flags & DKPW_REPLACE) != 0;
    cut = (u8)(flags >> 8);
    dead = 0;
    broken = 0;
#if PDW_TRACE
    pw_b_parent = pw_b_lookup = pw_b_mkdir = pw_b_open = pw_b_reserve = 0;
    pw_e_tail = pw_e_hash = pw_e_close = pw_e_info = pw_e_place = pw_e_rename = pw_e_journal = 0;
    pw_w_blk = pw_w_n = 0;
#endif
    PW_T0();
    at = parent_is_file(dest);
    PW_T1(pw_b_parent);
    if (at) return E_PATH;
    PW_T0();
    at = lookup(dest, &id);
    PW_T1(pw_b_lookup);
    if (at == 2) return E_PATH;
    if (at == 1) {
        if (!replace) return E_EXISTS;
        if (dk_fw_storage_file_id_open(id)) return E_FILE_IN_USE;
    }
    PW_T0();
    num = next_number(&there);   /* the folder of an earlier file of the session is reused */
    if (!there) f_mkdir(STAGE_DIR);
    PW_T1(pw_b_mkdir);
    names_for(num);
    PW_T0();
    i = (u32)dk_fw_storage_path_open((const u8 *)stage, (const u8 *)"w", hnd);
    PW_T1(pw_b_open);
    if (i != 0) {
        return E_WRITE;
    }
    st_open = 1;
    st_live = 1;
    total = size + ZERO_TAIL;
    written = 0;
    have_want = hash4 != 0;
    if (hash4) for (i = 0; i < 4; i++) want[i] = hash4[i];
    PW_T0();
    i = (u32)dk_fw_storage_file_reserve(hnd, 0, total);
    PW_T1(pw_b_reserve);
    if (i != 0) {
        dkpw_abort();
        return E_FULL;
    }
    return 0;
}

int32_t dkpw_write(const u8 *buf, u32 len)
{
    if (!st_open || dead || broken) return E_WRITE;
    if (len) {
        /* One firmware call per SDK write: the call has no size limit and ends with one inode write-back, and whole aligned 32 KiB pages go
         * straight to the card. A buffer that is not 16-byte aligned is bounced by the firmware (correct, only slower). */
        i32 wr;
        PW_T0();
        wr = dk_fw_storage_file_write(buf, len, hnd);
        PW_T1(pw_w_blk);
#if PDW_TRACE
        pw_w_n++;
#endif
        if (wr != (i32)len) {
            broken = 1; /* a retry would write the data again after a partial write */
            return E_WRITE;
        }
        written += len;
        if (cut_at(1)) return E_WRITE;
    }
    return 0;
}

/* Journal record `jour`: the destination path and its NUL, hashed so that a half-written record is told from a whole one. */
static int journal_write(void)
{
    int ok;
    u32 n = slen(dest) + 1;
    PW_I0();
    if (dk_fw_storage_path_open((const u8 *)jour, (const u8 *)"w", jh) != 0) {
        PW_I1(pw_e_journal);
        return 0;
    }
    ok = dk_fw_storage_file_write((const u8 *)dest, n, jh) == (i32)n && dk_fw_storage_file_hash(jh) == 0;
    dk_fw_storage_path_close(jh);
    if (!ok) f_delete(jour);
    PW_I1(pw_e_journal);
    return ok;
}

static i32 place(void)
{
    int journaled = 0, tries, at;
    u32 id;
    parents(dest);
    at = lookup(dest, &id);
    if (at == 2) {
        f_delete(stage);
        return E_PATH;
    }
    if (at == 1) {
        if (!replace) {
            f_delete(stage);
            return E_EXISTS;
        }
        if (dk_fw_storage_file_id_open(id)) {
            f_delete(stage);
            return E_FILE_IN_USE;
        }
        if (!journal_write()) {
            f_delete(stage);
            return E_WRITE;
        }
        journaled = 1;
        if (cut_at(3)) return E_WRITE;
        if (f_delete(dest) != 0) {   /* the stock firmware opened it meanwhile */
            f_delete(jour);
            f_delete(stage);
            return E_FILE_IN_USE;
        }
        if (cut_at(4)) return E_WRITE;
    }
    if (cut == 6) {
        dk_fw_storage_file_link((const u8 *)stage, (const u8 *)dest);
        dead = 1;
        return E_WRITE;
    }
    for (tries = 0; tries < 3; tries++) {
        int renamed;
        PW_I0();
        renamed = cut != 7 && dk_fw_storage_file_rename((const u8 *)stage, (const u8 *)dest) == 0;
        PW_I1(pw_e_rename);
        if (renamed) {
            if (cut_at(5)) return E_WRITE;
            if (journaled) f_delete(jour);
            return 0;
        }
    }
    /* A rename that failed after the destination was deleted: the journal and the staged file stay, and the runtime runs the recovery now. */
    if (!journaled) f_delete(stage);
    return E_WRITE;
}

static int32_t close_file(u8 *hash_out)
{
    i32 r;
    u32 i;
    if (dead) return E_WRITE;
    if (!st_open || broken || written + ZERO_TAIL != total) {
        int bad = broken;
        dkpw_abort();
        return bad ? E_WRITE : E_LENGTH;
    }
    PW_T0();
    r = dk_fw_storage_file_write(zeros, ZERO_TAIL, hnd);
    PW_T1(pw_e_tail);
    if (r != (i32)ZERO_TAIL) {
        dkpw_abort();
        return E_WRITE;
    }
    if (cut == 8) {
        /* a stock writer that died: the file reached its name with no hash */
        dk_fw_storage_path_close(hnd);
        st_open = 0;
        parents(dest);
        dk_fw_storage_file_rename((const u8 *)stage, (const u8 *)dest);
        dead = 1;
        return E_WRITE;
    }
    PW_T0();
    r = dk_fw_storage_file_hash(hnd);
    PW_T1(pw_e_hash);
    if (r != 0) {
        dkpw_abort();
        return E_WRITE;
    }
    PW_T0();
    dk_fw_storage_path_close(hnd);
    PW_T1(pw_e_close);
    st_open = 0;
    PW_T0();
    r = f_info(stage, inf);
    PW_T1(pw_e_info);
    if (r != 0 || inf[2] != total) {
        dkpw_abort();
        return E_WRITE;
    }
    if (hash_out) for (i = 0; i < 4; i++) hash_out[i] = (u8)(inf[1] >> (24 - 8 * i));
    if (have_want) {
        for (i = 0; i < 4; i++) {
            if (want[i] != (u8)(inf[1] >> (24 - 8 * i))) {
                dkpw_abort();
                return E_HASH;
            }
        }
    }
    if (cut_at(2)) return E_WRITE;
    PW_T0();
    r = place();
    PW_T1(pw_e_place);
    st_live = 0;   /* moved into place, or removed by `place`, or left for the recovery */
    return r;
}

int32_t dkpw_close(u8 *hash_out)
{
    int32_t r = close_file(hash_out);
    pw_emit();
    return r;
}

int32_t dkpw_delete(const u8 *path)
{
    u32 n = slen((const char *)path), i, id;
    int at;
    if (n == 0 || n + 1 >= DEST_MAX || in_factory((const char *)path)) return E_PATH;
    dest[0] = '/';
    for (i = 0; i <= n; i++) dest[1 + i] = (char)path[i];
    at = lookup(dest, &id);
    if (at == 0) return E_NOFILE;
    if (at == 2) return E_PATH;
    if (dk_fw_storage_file_id_open(id)) return E_FILE_IN_USE;
    return f_delete(dest) == 0 ? 0 : E_FILE_IN_USE;
}

/* ---- boot recovery ----------------------------------------------------------------------------------------------------------- */

static char names[NAMES][JOUR_LEN + 1];
static char recpath[DEST_MAX];

static void in_stage(char *out, const char *name)
{
    scopy(out, STAGE_DIR "/");
    scopy(out + slen(out), name);
}

/* Collect up to NAMES of our own names of the staging folder; the entries that are not ours are counted in `foreign` and left alone. Returns
 * the count, or -1 when the folder is not there. */
static i32 collect(u32 *foreign)
{
    u32 n = 0;
    int j;
    if (!dir_open(STAGE_DIR)) return -1;
    while (n < NAMES && dir_next()) {
        if (ours(ENT_NAME, &j)) scopy(names[n++], ENT_NAME);
        else (*foreign)++;
    }
    return (i32)n;
}

/* Read the journal `jp` into recpath: 1 when it is whole (hashed) and holds one absolute path. */
static int journal_read(const char *jp)
{
    i32 got;
    u32 i;
    if (f_info(jp, inf) != 0) return 0;
    if (dk_fw_storage_path_open((const u8 *)jp, (const u8 *)"r", rh) != 0) return 0;
    got = dk_fw_storage_path_read(jbuf, 256, rh);
    dk_fw_storage_path_close(rh);
    if (got < 3 || got >= 256 || jbuf[got - 1] != 0 || jbuf[0] != '/') return 0;
    for (i = 0; i < (u32)got; i++) recpath[i] = (char)jbuf[i];
    return slen(recpath) + 1 == (u32)got;
}

int32_t dkpw_recover(u32 *out)
{
    u32 round, k, foreign = 0;
    out[0] = out[1] = out[2] = out[3] = 0;
    if (!mounted()) return -1;
    if (dead) return 0;   /* a simulated power cut: nothing runs */
    for (round = 0; round < 64; round++) {
        u32 progress = 0, left = 0, id;
        i32 n;
        foreign = 0;
        n = collect(&foreign);
        if (n <= 0) break;
        for (k = 0; k < (u32)n; k++) {
            char sp[48], jp[56];
            int is_j = 0;
            ours(names[k], &is_j);
            in_stage(sp, names[k]);
            if (is_j) {
                in_stage(jp, names[k]);
                sp[slen(sp) - 5] = 0;   /* the staged file of this journal */
                if (journal_read(jp) && f_info(sp, inf) == 0 && lookup(recpath, &id) == 0) {
                    /* a hashed staged file and a destination that is gone: finish the replace */
                    parents(recpath);
                    if (dk_fw_storage_file_rename((const u8 *)sp, (const u8 *)recpath) == 0) {
                        out[0]++;
                        f_delete(jp);
                        progress++;
                    } else {
                        left++;
                    }
                    continue;
                }
                /* Anything else: the destination is there, or the journal or the staged file is not whole. The destination is either the old
                 * file (a cut before its delete) or the new file under two names (a cut inside the rename; the link count is 2). Deleting
                 * the staged name drops the new data in the first case and only the extra name in the second, never the destination's data. */
                if (f_delete(sp) == 0) out[1]++;
                if (f_delete(jp) == 0) out[1]++;
                progress++;
            } else {
                char jn[56];
                scopy(jn, sp);
                scopy(jn + slen(jn), ".dest");
                if (lookup(jn, &id)) continue;   /* its journal decides */
                if (f_delete(sp) == 0) out[1]++;
                progress++;
            }
        }
        out[2] += left;
        if (!progress || n < (i32)NAMES) break;
    }
    out[3] = foreign;
    tidy();
    return 0;
}
