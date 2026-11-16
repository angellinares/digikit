/* core/plusdrive-write on the Digitakt II 1.17: file writes to the stock +Drive through the firmware's own C-level file code (finding 120).
 * The plug-in point is runtime/include/dkrt_pdw.h; the session rules (owner, handle, offsets, expiry) are the runtime's.
 *
 * A file is written under the staging folder /.dkstage, complete with the 16 zero bytes the firmware stores after every file and its close hash,
 * and moved into place with the firmware's rename (the inode and its hash stay). A cut at any point leaves a half-written file only in the
 * staging folder; the boot recovery removes it. A replace is: write a hashed journal record naming the destination, delete the destination,
 * rename the staged file, delete the journal. The recovery finishes a replace whose destination is missing and drops one whose destination is
 * still there. The folder is created at the first write; the runtime removes it (`dkpw_recover`) once it is empty and no write session has run for 2 s,
 * not at the end of each file. A new file skips the staging (direct writes, below).
 *
 * Facts the order of steps relies on (finding 120, static, firmware 1.17):
 *  - Every inode, directory page and bitmap update is written through to the eMMC by the firmware's block write before the file call returns
 *    (FUN_4015b868, FUN_40155c34, FUN_4015bb7e call the block write, which waits for the transfer), in call order. No flush is needed between steps.
 *  - An inode has a link count (+2). Adding an entry (rename, link) writes the incremented count first; deleting a name removes the entry, then
 *    decrements the count, and frees the data only when it reaches 0. So one name of a file with two names can be deleted without freeing the
 *    data, and a cut inside a rename leaves two names with count 2.
 *  - Names are compared byte for byte (case-sensitive).
 *
 * The filesystem mutex of the firmware protects every call; this adapter keeps one set of state and the runtime makes one call at a time.
 *
 * Close hash (finding 120, "Close without the re-read"): the firmware's close hash FUN_4015b1e8 reads the whole file back through the page cache in
 * 32 KiB pieces (4.2 MB/s). `hash_fast` reads the same pages from the card with the firmware's block read (the card sector of a file page comes from
 * the firmware's extent lookup), hashes them with `pdw_hash.h`, compares the result with the host's hash (BEGIN's hash4) before it writes anything,
 * and then runs the rest of FUN_4015b1e8 in its order with the firmware's own functions under its mutex. PDW_FAST_CLOSE 0 builds the adapter with
 * the firmware's hash only.
 *
 * Speed levers (finding 134 has the hardware times; there is no switch for them):
 *  - Read-back in runs (D1): `hash_fast` reads the contiguous pages of one extent with one block read of up to RUN_PAGES pages (128 KiB; core/sdhc-large
 *    makes it one card command) into a buffer from the firmware's allocator and hashes the run in one piece. When that buffer cannot be had, it reads page
 *    by page into the firmware's scratch buffers.
 *  - Trailer in the last write (D2): when a data write completes the file, its last-page part and the 16 zero bytes go to the firmware in one call (through a
 *    bounce copy), so the close has no trailer write of its own. Content, size and hash are the same. Without a bounce buffer, or when the data ends on a
 *    page boundary, the close writes the trailer as before.
 *  - Page allocation (D3): `reserve_fast` allocates the pages of a new file with the firmware's page allocator (FUN_401581dc) and one inode write-back,
 *    and skips what `file_reserve` does besides: the free page count (a bit-by-bit loop over the 82 KiB bitmap). A short allocation falls back to
 *    the firmware's `file_reserve`, which answers the card-full error.
 *  - Direct writes (D4): a new file (destination missing, no REPLACE) is written at its final name and not renamed into place. There is no record and no
 *    boot cleanup: the file is visible, without a hash, from its creation to its close hash. A power cut in that time leaves that one partial file, without
 *    a hash, at its name, and nothing else changes. The next upload of the same path finds a file with no hash on the device and sends it with REPLACE
 *    (staged, journaled, renamed), which replaces it. A failed or aborted write deletes the file (`dkpw_abort`). A folder that `parents` created stays
 *    when the upload fails. */

#include <stddef.h>
#include <stdint.h>
#include "dkrt_pdw.h"
#include "dk_fw_storage.h"
#include "dk_fw_rtos.h"
#include "dk_fw_mem.h"
#include "pdw_hash.h"

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

/* Read-back run: 4 pages (128 KiB); the buffer holds two runs when the hash overlaps the next read */
#define RUN_PAGES 4u
#define RUN_BYTES (RUN_PAGES << 15)

/* 1: the close hash reads the card directly (`hash_fast`); 0: the firmware's own close hash only. */
#define PDW_FAST_CLOSE 1
#define STAGE_DIR "/.dkstage"
#define DEST_MAX 256u
#define NAMES 24u
#define ZERO_TAIL 16u
/* The firmware's file geometry (finding 120): a file page is 32 KiB (64 sectors of 512 bytes) and a hash page holds the 8192 inode hash words of 32 KiB */
#define SECTOR_BYTES 512u
#define PAGE_SHIFT 15u
#define PAGE_BYTES 0x8000u
#define PAGE_SECTOR_SHIFT 6u /* sectors per page, as a shift */
#define PAGE_FIND_STATUS                                                                           \
    0xffu /* the low byte of the result of the page lookup: 0 when the page was found */
#define HASH_PAGE_SHIFT 13u
#define HASH_PAGE_WORDS 0x2000u
#define HASH_PAGE_SECTORS 0x40u
/* hash_fast: the card's pages could not be read the way it needs (nothing was changed): the close hash falls back to the firmware's own */
#define HF_FALLBACK 1
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
    u32 phys, phys2; /* the card page of a file page (page_find); of the next one (run length) */
    u8 pad[640 - 256 - 96 - 0x114 - 8];
};
struct dma DKPW_DMA __attribute__((aligned(16)));
_Static_assert(sizeof(struct dma) == 640,
               "DKPW_DMA is 640 bytes (tests/regression/runtime/write-safety.ts)");
_Static_assert(offsetof(struct dma, jbuf) % 16 == 0 && offsetof(struct dma, hnd) % 16 == 0 &&
                   offsetof(struct dma, jh) % 16 == 0 && offsetof(struct dma, rh) % 16 == 0 &&
                   offsetof(struct dma, inf) % 16 == 0 && offsetof(struct dma, inf2) % 16 == 0 &&
                   offsetof(struct dma, dstate) % 16 == 0 && offsetof(struct dma, dout) % 16 == 0,
               "buffers handed to the firmware are 16-byte aligned");
#define hnd (DKPW_DMA.hnd)
#define jh (DKPW_DMA.jh)
#define rh (DKPW_DMA.rh)
#define inf (DKPW_DMA.inf)
#define dstate (DKPW_DMA.dstate)
#define dout (DKPW_DMA.dout)
#define jbuf (DKPW_DMA.jbuf)

static u8 st_open; /* `hnd` is an open staged file */
static u8 st_live; /* a staged file exists and the caller still owns it */
static u8 broken;  /* a write failed: nothing more is written to this file */
static u8 have_want, replace;
static u8 want[4];
/* Test knob (bits 8 to 15 of the flags of dkpw_begin, set by the runtime's DKRT_PD_CUT): the adapter stops dead at this step of the write, as a
 * power cut would: 1 after the first data write, 2 after the hash and close, 3 after the journal, 4 after the destination is deleted, 5 after
 * the rename, 6 between the two halves of the rename (a second name added, the staged name still there). From then on every call answers E_WRITE
 * and touches nothing, so that the card is the state a cut leaves. 7 is no cut: the rename fails (as the firmware could refuse it) and the
 * recovery must finish the replace. 9 fails after the hash page was written (then restored; the close hash only). 10 makes `hash_fast` give up before it reads or writes anything (as a page lookup that fails would), so the close runs the firmware's own hash. 8 leaves an unhashed file at the destination (a stock writer that died): no hash, rename, dead. */
static u8 cut, dead;
/* Test knob: bit 7 of the knob byte (DKRT_PD_CUT = 0x80, with or without a cut step in the low bits) closes with the firmware's own hash instead of
 * `hash_fast`; the tests write the same file through both and compare the card and the RAM hash table. */
static u8 slow;
/* 1: `hash_fast` wrote hash pages and inode words but did not rebuild the firmware's RAM hash table (`rebuild_table` does, once per upload batch). */
static u8 rb_pending;
static u32 total, written;
static char stage[40], jour[48];
/* Direct write: the file is written at `dest`, not staged (`spath`). */
static u8 direct;
/* The 16 zero bytes were written with the last data */
static u8 tail_done;
static char dest[DEST_MAX] __attribute__((aligned(16)));
static char lp[DEST_MAX];
static const u8 zeros[ZERO_TAIL] __attribute__((aligned(16)));

#define PDW_TRACE 0 /* 1 for hardware measurement builds: per-step times in the log at close */
#if PDW_TRACE
/* the runtime (dkrt.h) */
extern u32 dkrt_blocks(void); /* the 1500 Hz block counter: 1 unit = 0.667 ms */
extern void dk_log_event(u32 kind, u32 flags, const u32 *a);
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
    dk_log_event(PW_KIND_MOD_LOG, PW_FLAGS, a);
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
    if (cut == step)
        dead = 1;
    return dead;
}

u32 dkpw_version(void)
{
    return DKPW_VERSION;
}
const u8 *dkpw_staging(void)
{
    return (const u8 *)STAGE_DIR;
}

static u32 slen(const char *s)
{
    u32 n = 0;
    while (s[n])
        n++;
    return n;
}

static void scopy(char *d, const char *s)
{
    u32 i = 0;
    while ((d[i] = s[i]) != 0)
        i++;
}

static int seq(const char *a, const char *b)
{
    u32 i = 0;
    while (a[i] == b[i]) {
        if (!a[i])
            return 1;
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

static i32 f_delete(const char *p)
{
    return dk_fw_storage_file_delete((const u8 *)p);
}
static i32 f_info(const char *p, u32 *out)
{
    return dk_fw_storage_file_info((const u8 *)p, out);
}
static i32 f_mkdir(const char *p)
{
    return dk_fw_storage_file_mkdir((const u8 *)p);
}

/* The firmware's mounted flag: every path function of the filesystem starts by testing it. */
static int mounted(void)
{
    return *(const volatile u32 *)lit(&dk_fw_storage_mounted) != 0;
}

/* ---- directory scans ---------------------------------------------------------------------------------------------------------- */

#define ENT_NAME ((const char *)dout + 20)
#define ENT_ID (dout[0])
#define ENT_FOLDER (*(const volatile uint16_t *)((const u8 *)dout + 4) == 1)

/* Open directory `p`; 1 when it is there. */
static int dir_open(const char *p)
{
    return dk_fw_storage_directory_open((const u8 *)p, dstate) == 0;
}

/* The next entry in `dout` (never "." or ".."); 0 at the end. */
static int dir_next(void)
{
    u32 i;
    for (;;) {
        const char *nm;
        for (i = 0; i < 0x114 / 4; i++)
            dout[i] = 0;
        if (dk_fw_storage_directory_next(dstate, (u8 *)dout) != 1)
            return 0;
        nm = ENT_NAME;
        if (nm[0] == '.' && (nm[1] == 0 || (nm[1] == '.' && nm[2] == 0)))
            continue;
        return 1;
    }
}

/* What is at the absolute path `p`: 0 nothing (or the parent is not there), 1 a file (hashed or not), 2 a folder; `id` gets its inode id.
 * file_info sees neither folders nor unhashed files, so this scans the parent folder. */
static int lookup(const char *p, u32 *id)
{
    u32 n = slen(p), last = 0, i;
    if (n >= DEST_MAX)
        return 0;
    for (i = 0; i < n; i++)
        if (p[i] == '/')
            last = i;
    scopy(lp, p);
    if (last == 0)
        scopy(lp, "/");
    else
        lp[last] = 0;
    if (!dir_open(lp))
        return 0;
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

static int hexv(char c)
{
    return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f');
}

/* Is `s` one of our names: "s" and 8 hex digits, or that and ".dest" (`is_journal`)? */
static int ours(const char *s, int *is_journal)
{
    u32 i, n = slen(s);
    if (n != STAGE_LEN && n != JOUR_LEN)
        return 0;
    if (s[0] != 's')
        return 0;
    for (i = 1; i < STAGE_LEN; i++)
        if (!hexv(s[i]))
            return 0;
    *is_journal = n == JOUR_LEN;
    return n == STAGE_LEN || seq(s + STAGE_LEN, ".dest");
}

static u32 hexval(const char *s)
{
    u32 v = 0, i;
    for (i = 1; i < STAGE_LEN; i++)
        v = (v << 4) | (u32)(s[i] <= '9' ? s[i] - '0' : s[i] - 'a' + 10);
    return v;
}

static void names_for(u32 n)
{
    static const char hex[] = "0123456789abcdef";
    u32 i;
    scopy(stage, STAGE_DIR "/s");
    for (i = 0; i < 8; i++)
        stage[11 + i] = hex[(n >> (28 - 4 * i)) & 15];
    stage[19] = 0;
    scopy(jour, stage);
    scopy(jour + 19, ".dest");
}

/* The staging folder is empty (or not there)? */
static int staging_empty(void)
{
    if (!dir_open(STAGE_DIR))
        return 1;
    return !dir_next();
}

/* Remove the folder when it is empty. Only the boot recovery and the runtime's idle call (`dkpw_recover`) do it: the folder lives for a whole
 * upload session, because its creation (about 420 ms) and removal (about 150 ms) cost more than a small file. */
static void tidy(void)
{
    if (!dead && staging_empty())
        dk_fw_storage_file_rmdir((const u8 *)STAGE_DIR);
}

/* The file being written: the staged name, or the destination for a direct write. */
static const char *spath(void)
{
    return direct ? dest : stage;
}

/* The folder /factory (and what is under it) is the firmware's own read-only ROM tree: its inodes and directory pages are RAM tables built at every
 * mount (ids from 0x1000000), never written to the card. Nothing is written, replaced or deleted there. */
static int in_factory(const char *p)
{
    static const char f[] = "factory";
    u32 i;
    for (i = 0; f[i]; i++)
        if (p[i] != f[i])
            return 0;
    return p[i] == 0 || p[i] == '/';
}

/* ---- write ------------------------------------------------------------------------------------------------------------------- */

/* The firmware's RAM hash table (FUN_4015b0ee reads all 44 hash pages: about 300 ms) is derived data, rebuilt at every mount. `hash_fast` leaves it
 * stale and sets `rb_pending`; the rebuild runs once when the batch ends (`dkpw_recover`, which the runtime calls after 2 s without a session, and
 * `dkpw_abort`). A stale table only misses the new files: a lookup by hash (sample references of a project) does not find them yet, and the
 * removal of an entry (inode free, FUN_4015b032) ignores a (hash, id) pair it does not find. The table is sorted at every rebuild and only shrinks
 * meanwhile, so it stays sorted. Under the filesystem mutex, as in the firmware (FUN_4015b3aa). */
static void rebuild_table(void)
{
    u32 *mutex;
    if (!rb_pending || dead || !mounted())
        return;
    mutex = (u32 *)lit(&dk_fw_storage_fs_mutex);
    dk_fw_rtos_mutex_lock(mutex);
    dk_fw_storage_hash_table_rebuild();
    dk_fw_rtos_mutex_unlock(mutex);
    rb_pending = 0;
}

/* The runtime's dk_files_commit: the table rebuild that a batch of files left pending, without waiting for the 2 s idle recovery. */
int32_t dkpw_commit(void)
{
    rebuild_table();
    return 0;
}

void dkpw_abort(void)
{
    if (dead)
        return;
    if (st_open) {
        dk_fw_storage_path_close(hnd);
        st_open = 0;
    }
    if (st_live) {
        f_delete(spath());
        st_live = 0;
    }
    rebuild_table();
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
                if (!any || v > top)
                    top = v;
                any = 1;
            }
        }
    }
    return any ? top + 1 : 0;
}

/* The pages of the new staged file, as the firmware's file_reserve (FUN_40158448) does for a file without extents, minus its free page count: the page
 * allocator (FUN_401581dc: first fit from the bitmap start, one extent per allocation) and one inode write-back, under the filesystem mutex. 0 when the
 * file's last page has a card page afterwards (the allocator allocates the pages in order and stops when none is free). Anything else, and a file that
 * already has extents, is nothing to this function (1: nothing is changed that `file_reserve` would not also find): the caller runs `file_reserve`, which
 * allocates what is missing and answers -1 when the card cannot hold it (the caller then deletes the file, and its pages with it). Hint 0 as the firmware. */
static u32 reserve_fast(void)
{
    u32 *mutex = (u32 *)lit(&dk_fw_storage_fs_mutex);
    u32 id = hnd[0], pages = (total + (PAGE_BYTES - 1u)) >> PAGE_SHIFT;
    u32 r = 1;
    u8 *ino;
    if (!mounted() || id > 0xffffffu)
        return 1;
    dk_fw_rtos_mutex_lock(mutex);
    ino = dk_fw_storage_inode_get(id);
    if (ino) {
        if (*(const uint16_t *)(ino + 0x1e) == 0) { /* no extents yet */
            dk_fw_storage_file_alloc_pages(ino, 0, pages, 0);
            dk_fw_storage_inode_write(ino);
            if ((dk_fw_storage_file_page_find(ino, pages - 1u, &DKPW_DMA.phys) &
                 PAGE_FIND_STATUS) != 0)
                r = 0;
        }
        dk_fw_storage_inode_put(ino);
    }
    dk_fw_rtos_mutex_unlock(mutex);
    return r;
}

int32_t dkpw_begin(const u8 *path, u32 size, const u8 *hash4, u32 flags)
{
    u32 n = slen((const char *)path), i, id;
    int at, there;
    u32 num;
    if (n == 0 || n + 1 >= DEST_MAX || in_factory((const char *)path))
        return E_PATH;
    if (st_live || st_open)
        dkpw_abort();
    dest[0] = '/';
    for (i = 0; i <= n; i++)
        dest[1 + i] = (char)path[i];
    replace = (flags & DKPW_REPLACE) != 0;
    cut = (u8)(flags >> 8);
    slow = (cut & 0x80u) != 0;
    cut &= 0x7fu;
    dead = 0;
    broken = 0;
    direct = 0;
    tail_done = 0;
#if PDW_TRACE
    pw_b_parent = pw_b_lookup = pw_b_mkdir = pw_b_open = pw_b_reserve = 0;
    pw_e_tail = pw_e_hash = pw_e_close = pw_e_info = pw_e_place = pw_e_rename = pw_e_journal = 0;
    pw_w_blk = pw_w_n = 0;
#endif
    PW_T0();
    at = parent_is_file(dest);
    PW_T1(pw_b_parent);
    if (at)
        return E_PATH;
    PW_T0();
    at = lookup(dest, &id);
    PW_T1(pw_b_lookup);
    if (at == 2)
        return E_PATH;
    if (at == 1) {
        if (!replace)
            return E_EXISTS;
        if (dk_fw_storage_file_id_open(id))
            return E_FILE_IN_USE;
    }
    PW_T0();
    if (at == 0 && !replace && cut != 8) {
        /* a new file: written at its final name */
        parents(dest);
        direct = 1;
    }
    if (!direct) {
        num = next_number(&there); /* the folder of an earlier file of the session is reused */
        if (!there)
            f_mkdir(STAGE_DIR);
        names_for(num);
    }
    PW_T1(pw_b_mkdir);
    PW_T0();
    i = (u32)dk_fw_storage_path_open((const u8 *)spath(), (const u8 *)"w", hnd);
    PW_T1(pw_b_open);
    if (i != 0) {
        return E_WRITE;
    }
    st_open = 1;
    st_live = 1;
    total = size + ZERO_TAIL;
    written = 0;
    have_want = hash4 != 0;
    if (hash4)
        for (i = 0; i < 4; i++)
            want[i] = hash4[i];
    PW_T0();
    i = reserve_fast();
    if (i != 0)
        i = (u32)dk_fw_storage_file_reserve(hnd, 0, total);
    PW_T1(pw_b_reserve);
    if (i != 0) {
        dkpw_abort();
        return E_FULL;
    }
    return 0;
}

/* Push and invalidate the data cache lines of [p, p + n): dirty lines are written back before a DMA and no stale line is left to be refilled or evicted over
 * DMA data. The firmware's block read (FUN_4012e1b6) does the same for its destination before a direct DMA (its cpushl loop at 0x4012e27e, 16-byte aligned
 * destinations only) and copies from a bounce buffer otherwise; it does nothing after the DMA. */
static void cache_push(const u8 *p, u32 n)
{
    for (n = (n + 15u) >> 4; n; n--, p += 16)
        __asm__ volatile("cpushl %%dc,(%0)" : : "a"(p) : "memory");
}

/* The last data write with the 16 zero bytes of the trailer: `len` bytes at file offset `written` end the data. The whole pages go to the
 * firmware straight from `buf`; the rest (under 32 KiB) and the zeros go in one call from a bounce copy, so the page the file ends in is written once
 * (the trailer write of the close would write it again). 1 when both calls wrote everything (the trailer is done), 0 when the bounce buffer could not be
 * had and nothing was written (the caller writes `buf` as it is), -1 when a write failed. */
static i32 write_with_tail(const u8 *buf, u32 len)
{
    u32 end = written + len;
    u32 keep = end & (PAGE_BYTES - 1u); /* bytes of the last page */
    u32 head = len >= keep ? len - keep : 0;
    u32 rest = len - head, n, i;
    u8 *bounce;
    i32 wr;
    if (keep == 0)
        return 0; /* the data ends on a page boundary: the trailer is a page of its own */
    bounce = dk_fw_mem_new(rest + ZERO_TAIL);
    if (!bounce)
        return 0;
    if ((uintptr_t)bounce & 15u) {
        dk_fw_mem_free(bounce);
        return 0;
    }
    if (head) {
        wr = dk_fw_storage_file_write(buf, head, hnd);
        if (wr != (i32)head) {
            dk_fw_mem_free(bounce);
            return -1;
        }
    }
    for (i = 0; i < rest; i++)
        bounce[i] = buf[head + i];
    for (i = 0; i < ZERO_TAIL; i++)
        bounce[rest + i] = 0;
    n = rest + ZERO_TAIL;
    cache_push(bounce, n); /* the firmware writes whole pages from the buffer by DMA */
    wr = dk_fw_storage_file_write(bounce, n, hnd);
    dk_fw_mem_free(bounce);
    return wr == (i32)n ? 1 : -1;
}

int32_t dkpw_write(const u8 *buf, u32 len)
{
    if (!st_open || dead || broken)
        return E_WRITE;
    if (len) {
        /* One firmware call per SDK write: the call has no size limit and ends with one inode write-back, and whole aligned 32 KiB pages go
         * straight to the card. A buffer that is not 16-byte aligned is bounced by the firmware (correct, only slower). */
        i32 wr;
        PW_T0();
        wr = -1;
        if (written + len + ZERO_TAIL == total) {
            i32 t = write_with_tail(buf, len);
            if (t != 0) {
                wr = t > 0 ? (i32)len : -1;
                tail_done = (u8)(t > 0);
            } else {
                wr = dk_fw_storage_file_write(buf, len, hnd);
            }
        } else {
            wr = dk_fw_storage_file_write(buf, len, hnd);
        }
        PW_T1(pw_w_blk);
#if PDW_TRACE
        pw_w_n++;
#endif
        if (wr != (i32)len) {
            broken = 1; /* a retry would write the data again after a partial write */
            return E_WRITE;
        }
        written += len;
        if (cut_at(1))
            return E_WRITE;
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
    ok = dk_fw_storage_file_write((const u8 *)dest, n, jh) == (i32)n &&
         dk_fw_storage_file_hash(jh) == 0;
    dk_fw_storage_path_close(jh);
    if (!ok)
        f_delete(jour);
    PW_I1(pw_e_journal);
    return ok;
}

static i32 place(void)
{
    int journaled = 0, tries, at;
    u32 id;
    if (direct)
        return 0; /* written at its name, hashed: nothing to move */
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
        if (cut_at(3))
            return E_WRITE;
        if (f_delete(dest) != 0) { /* the stock firmware opened it meanwhile */
            f_delete(jour);
            f_delete(stage);
            return E_FILE_IN_USE;
        }
        if (cut_at(4))
            return E_WRITE;
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
            if (cut_at(5))
                return E_WRITE;
            if (journaled)
                f_delete(jour);
            return 0;
        }
    }
    /* A rename that failed after the destination was deleted: the journal and the staged file stay, and the runtime runs the recovery now. */
    if (!journaled)
        f_delete(stage);
    return E_WRITE;
}

#if PDW_FAST_CLOSE
/* The runtime's helper task and speed switches (dkrt.h). Lever C (bit 2 of the switches): the hash of file page p runs on the helper task while page p + 1 is
 * read, which blocks this task on the card; the two pages alternate between the firmware's two 32 KiB buffers. */
extern i32 dkrt_aux_run(uintptr_t f, u8 *ctx);
extern i32 dkrt_aux_wait(void);
extern u32 dkrt_speed_flags(void);
#define SPEED_OVERLAP 4u

struct hjob {
    struct pdw_hash *h;
    const u8 *buf;
    u32 n;
};

static void hjob_run(u8 *ctx)
{
    struct hjob *j = (struct hjob *)ctx;
    pdw_hash_update(j->h, j->buf, j->n);
}

/* The firmware's close hash of the staged file (FUN_4015b1e8) without its re-read through the page cache. 0; E_HASH when the card's data differs from
 * the host's hash (nothing is changed then); HF_FALLBACK when the card cannot be read the way this function needs (not mounted, no inode, a size that is not what
 * was written, a page that the extent lookup misses, a failed block read; nothing is changed yet), and the caller closes with the firmware's own hash;
 * E_WRITE when the hash page could not be written (the page is restored; the table, the inode and the event are not touched). Everything runs under the filesystem
 * mutex, as in the firmware (FUN_4015b3aa). The firmware's own scratch buffers are used: both are 32 KiB, 16-byte aligned and used only under that mutex
 * (the hash page buffer is free until the hash is done, so it is the second read buffer of the overlapped hash). */
static i32 hash_fast_run(u8 *big);

/* core/sdhc-large's range of the buffer whose card transfers may be large commands (the symbol is 0 when that module is not in the image) */
extern void sdhc_large_range(u32 lo, u32 hi);
static void large_range(u32 lo, u32 hi)
{
    void (*f)(u32, u32) = sdhc_large_range;
    __asm__("" : "+r"(f));
    if (f)
        f(lo, hi);
}

/* The runs of the read-back are read into one buffer of the firmware's allocator (two runs with the overlap), else page by page into the
 * firmware's scratch buffers. The allocation is made before the filesystem mutex is taken. */
static i32 hash_fast(void)
{
    u32 pages = (total + (PAGE_BYTES - 1u)) >> PAGE_SHIFT;
    u32 ovl = pages > 1u && (dkrt_speed_flags() & SPEED_OVERLAP) != 0;
    u8 *big = 0;
    i32 r;
    if (pages > 1u) {
        big = dk_fw_mem_new(ovl ? 2u * RUN_BYTES : RUN_BYTES);
        if (big && ((uintptr_t)big & 15u)) {
            dk_fw_mem_free(big);
            big = 0;
        }
    }
    r = hash_fast_run(big);
    if (big)
        dk_fw_mem_free(big);
    return r;
}

static i32 hash_fast_run(u8 *big)
{
    struct pdw_hash h;
    u32 *mutex = (u32 *)lit(&dk_fw_storage_fs_mutex);
    u8 *rbuf = (u8 *)lit(&dk_fw_storage_hash_read_buf);
    u8 *pbuf = (u8 *)lit(&dk_fw_storage_hash_page_buf);
    u32 hsec = lit(&dk_fw_storage_hash_page_sector);
    u32 dsec = lit(&dk_fw_storage_data_sector_base);
    u32 id = hnd[0], pages = (total + (PAGE_BYTES - 1u)) >> PAGE_SHIFT, p, hv, i, sector, old;
    u32 *ino;
    i32 r = HF_FALLBACK;
    struct hjob job;
    u32 pending = 0;
    u32 ovl = pages > 1u && (dkrt_speed_flags() & SPEED_OVERLAP) != 0;
    u32 ranged = 0, idx = 0;
    if (!mounted())
        return HF_FALLBACK;
    dk_fw_rtos_mutex_lock(mutex);
    ino = (u32 *)dk_fw_storage_inode_get(id);
    if (!ino)
        goto unlock;
    if (ino[1] != total || cut == 10)
        goto put; /* the inode size is what was written (knob 10 acts as a size mismatch) */
    pdw_hash_init(&h);
    job.h = &h;
    if (big) {
        cache_push(big, ovl ? 2u * RUN_BYTES : RUN_BYTES);
        large_range((u32)big, (u32)big + (ovl ? 2u * RUN_BYTES : RUN_BYTES));
        ranged = 1;
    } else {
        cache_push(rbuf, PAGE_BYTES);
        if (ovl)
            cache_push(pbuf, PAGE_BYTES);
    }
    for (p = 0; p < pages; idx++) {
        u32 run = 1, n;
        u8 *buf = (ovl && (idx & 1u)) ? pbuf : rbuf;
        if (big)
            buf = big + ((ovl && (idx & 1u)) ? RUN_BYTES : 0u);
        if ((dk_fw_storage_file_page_find((u8 *)ino, p, &DKPW_DMA.phys) & PAGE_FIND_STATUS) == 0) {
            if (pending)
                dkrt_aux_wait();
            goto put;
        }
        /* the run: the next pages that follow on the card (one extent), at most RUN_PAGES */
        while (big && run < RUN_PAGES && p + run < pages &&
               (dk_fw_storage_file_page_find((u8 *)ino, p + run, &DKPW_DMA.phys2) &
                PAGE_FIND_STATUS) != 0 &&
               DKPW_DMA.phys2 == DKPW_DMA.phys + run)
            run++;
        n = total - (p << PAGE_SHIFT);
        if (n > run << PAGE_SHIFT)
            n = run << PAGE_SHIFT;
        if (dk_fw_storage_block_read(dsec + (DKPW_DMA.phys << PAGE_SECTOR_SHIFT),
                                     (n + (SECTOR_BYTES - 1u)) & ~(SECTOR_BYTES - 1u), buf) != 0) {
            if (pending)
                dkrt_aux_wait();
            goto put;
        }
        if (pending) { /* the run before is hashed: its buffer is free again, and the hash state is ours */
            dkrt_aux_wait();
            pending = 0;
        }
        job.buf = buf;
        job.n = n;
        if (ovl && dkrt_aux_run((uintptr_t)hjob_run, (u8 *)&job) == 0)
            pending = 1;
        else
            pdw_hash_update(&h, buf, n);
        p += run;
    }
    if (ranged) {
        large_range(0, 0);
        ranged = 0;
    }
    if (pending)
        dkrt_aux_wait();
    hv = pdw_hash_finish(&h) | 1u;
    if (have_want) {
        for (i = 0; i < 4; i++) {
            if (want[i] != (u8)(hv >> (24 - 8 * i))) {
                r = E_HASH;
                goto put;
            }
        }
    }
    /* the rest of FUN_4015b1e8, in its order: the hash page, (the table: `rebuild_table`, later), the inode word, the event, the inode write-back */
    r = HF_FALLBACK;
    sector = hsec + (id >> HASH_PAGE_SHIFT) * HASH_PAGE_SECTORS;
    if (dk_fw_storage_block_read(sector, PAGE_BYTES, pbuf) != 0)
        goto put;
    old = ((u32 *)pbuf)[id & (HASH_PAGE_WORDS - 1u)];
    ((u32 *)pbuf)[id & (HASH_PAGE_WORDS - 1u)] = hv;
    if (dk_fw_storage_block_write(sector, PAGE_BYTES, pbuf) != 0 || cut == 9) {
        /* The staged file is deleted by the caller: no hash entry may point at its inode. Write the page back with the word as it was
         * (a failed write may have reached the card in part). The table, the inode and the event are not touched yet. */
        ((u32 *)pbuf)[id & (HASH_PAGE_WORDS - 1u)] = old;
        dk_fw_storage_block_write(sector, PAGE_BYTES, pbuf);
        r = E_WRITE;
        goto put;
    }
    rb_pending = 1; /* the table is rebuilt when the batch ends (`rebuild_table`) */
    ino[3] = hv;
    dk_fw_storage_event_post(3, ino[2], id);
    dk_fw_storage_inode_write((u8 *)ino);
    r = 0;
put:
    if (ranged)
        large_range(0, 0);
    dk_fw_storage_inode_put((u8 *)ino);
unlock:
    dk_fw_rtos_mutex_unlock(mutex);
    return r;
}
#endif

static int32_t close_file(u8 *hash_out)
{
    i32 r;
    u32 i;
    if (dead)
        return E_WRITE;
    if (!st_open || broken || written + ZERO_TAIL != total) {
        int bad = broken;
        dkpw_abort();
        return bad ? E_WRITE : E_LENGTH;
    }
    PW_T0();
    r = tail_done ? (i32)ZERO_TAIL : dk_fw_storage_file_write(zeros, ZERO_TAIL, hnd);
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
#if PDW_FAST_CLOSE
    r = HF_FALLBACK;
    if (!slow) {
        r = hash_fast();
        PW_T1(pw_e_hash);
        if (r != 0 && r != HF_FALLBACK) {
            dkpw_abort();
            return (int32_t)r;
        }
    }
    if (r == HF_FALLBACK)
#endif
    {
        r = dk_fw_storage_file_hash(hnd); /* rebuilds the table itself */
        PW_T1(pw_e_hash);
        if (r != 0) {
            dkpw_abort();
            return E_WRITE;
        }
    }
    PW_T0();
    dk_fw_storage_path_close(hnd);
    PW_T1(pw_e_close);
    st_open = 0;
    PW_T0();
    r = f_info(spath(), inf);
    PW_T1(pw_e_info);
    if (r != 0 || inf[2] != total) {
        dkpw_abort();
        return E_WRITE;
    }
    if (hash_out)
        for (i = 0; i < 4; i++)
            hash_out[i] = (u8)(inf[1] >> (24 - 8 * i));
    if (have_want) {
        for (i = 0; i < 4; i++) {
            if (want[i] != (u8)(inf[1] >> (24 - 8 * i))) {
                dkpw_abort();
                return E_HASH;
            }
        }
    }
    if (cut_at(2))
        return E_WRITE;
    PW_T0();
    r = place();
    PW_T1(pw_e_place);
    st_live = 0; /* moved into place, or removed by `place`, or left for the recovery */
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
    if (n == 0 || n + 1 >= DEST_MAX || in_factory((const char *)path))
        return E_PATH;
    dest[0] = '/';
    for (i = 0; i <= n; i++)
        dest[1 + i] = (char)path[i];
    at = lookup(dest, &id);
    if (at == 0)
        return E_NOFILE;
    if (at == 2)
        return E_PATH;
    if (dk_fw_storage_file_id_open(id))
        return E_FILE_IN_USE;
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
    if (!dir_open(STAGE_DIR))
        return -1;
    while (n < NAMES && dir_next()) {
        if (ours(ENT_NAME, &j))
            scopy(names[n++], ENT_NAME);
        else
            (*foreign)++;
    }
    return (i32)n;
}

/* Read the journal `jp` into recpath: 1 when it is whole (hashed) and holds one absolute path of at least `min` bytes with its NUL. */
static int journal_read(const char *jp, i32 min)
{
    i32 got;
    u32 i;
    if (f_info(jp, inf) != 0)
        return 0;
    if (dk_fw_storage_path_open((const u8 *)jp, (const u8 *)"r", rh) != 0)
        return 0;
    got = dk_fw_storage_path_read(jbuf, 256, rh);
    dk_fw_storage_path_close(rh);
    if (got < min || got >= 256 || jbuf[got - 1] != 0 || jbuf[0] != '/')
        return 0;
    for (i = 0; i < (u32)got; i++)
        recpath[i] = (char)jbuf[i];
    return slen(recpath) + 1 == (u32)got;
}

int32_t dkpw_recover(u32 *out)
{
    u32 round, k, foreign = 0;
    out[0] = out[1] = out[2] = out[3] = 0;
    if (!mounted())
        return -1;
    if (dead)
        return 0;    /* a simulated power cut: nothing runs */
    rebuild_table(); /* the end of an upload batch */
    for (round = 0; round < 64; round++) {
        u32 progress = 0, left = 0, id;
        i32 n;
        foreign = 0;
        n = collect(&foreign);
        if (n <= 0)
            break;
        for (k = 0; k < (u32)n; k++) {
            char sp[48], jp[56];
            int is_j = 0;
            ours(names[k], &is_j);
            in_stage(sp, names[k]);
            if (is_j) {
                in_stage(jp, names[k]);
                sp[slen(sp) - 5] = 0; /* the staged file of this journal */
                if (journal_read(jp, 3) && f_info(sp, inf) == 0 && lookup(recpath, &id) == 0) {
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
                if (f_delete(sp) == 0)
                    out[1]++;
                if (f_delete(jp) == 0)
                    out[1]++;
                progress++;
            } else {
                char jn[56];
                scopy(jn, sp);
                scopy(jn + slen(jn), ".dest");
                if (lookup(jn, &id))
                    continue; /* its journal decides */
                if (f_delete(sp) == 0)
                    out[1]++;
                progress++;
            }
        }
        out[2] += left;
        if (!progress || n < (i32)NAMES)
            break;
    }
    out[3] = foreign;
    tidy();
    return 0;
}
