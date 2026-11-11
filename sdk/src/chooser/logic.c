#include <dk/chooser/logic.h>
#include <dk/sample/pool.h>

static char lower(char c)
{
    return c >= 'A' && c <= 'Z' ? (char)(c + 32) : c;
}

static int slen(const char *s)
{
    int n = 0;
    while (s[n])
        n++;
    return n;
}

/* Appends s to out at *n, keeping room for the NUL in cap bytes. */
static void put(char *out, int cap, int *n, const char *s)
{
    while (*s && *n < cap - 1)
        out[(*n)++] = *s++;
    out[*n] = 0;
}

void dk_text_cat(char *out, const char *a, const char *b)
{
    char t[DK_CHOOSER_TEXT + 1];
    int n = 0;
    put(t, DK_CHOOSER_TEXT + 1, &n, a);
    put(t, DK_CHOOSER_TEXT + 1, &n, b);
    for (n = 0; t[n]; n++)
        out[n] = t[n];
    out[n] = 0;
}

int dk_text_utoa(char *out, uint32_t v)
{
    char t[10];
    int n = 0, i = 0;
    do {
        t[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (n)
        out[i++] = t[--n];
    out[i] = 0;
    return i;
}

int dk_chooser_row_before(const dk_chooser_row *a, const dk_chooser_row *b)
{
    int i;
    if (a->dir != b->dir)
        return a->dir > b->dir;
    for (i = 0; a->name[i] || b->name[i]; i++) {
        char x = lower(a->name[i]), y = lower(b->name[i]);
        if (x != y)
            return (uint8_t)x < (uint8_t)y;
    }
    for (i = 0; a->name[i] || b->name[i]; i++)
        if (a->name[i] != b->name[i])
            return (uint8_t)a->name[i] < (uint8_t)b->name[i];
    return 0;
}

int dk_chooser_row_insert(dk_chooser_row *rows, uint8_t *idx, int *n, int cap,
                          const dk_chooser_row *r)
{
    int slot, j, i;
    if (*n < cap)
        slot = (*n)++;
    else {
        if (!dk_chooser_row_before(r, &rows[idx[cap - 1]]))
            return 0;
        slot = idx[cap - 1];
    }
    for (i = 0; i < DK_CHOOSER_NAME; i++)
        rows[slot].name[i] = r->name[i];
    rows[slot].size = r->size;
    rows[slot].dir = r->dir;
    /* idx[*n - 1] is the hole: the new row's place or the dropped row's */
    for (j = *n - 1; j > 0 && dk_chooser_row_before(&rows[slot], &rows[idx[j - 1]]); j--)
        idx[j] = idx[j - 1];
    idx[j] = (uint8_t)slot;
    return 1;
}

int dk_path_join(char *out, int cap, const char *dir, const char *leaf)
{
    int n = 0, dl = slen(dir), ll = slen(leaf);
    int total = dl ? dl + 1 + ll : ll;
    if (total > 255 || total + 1 > cap || ll == 0)
        return -1;
    if (dl) {
        put(out, cap, &n, dir);
        out[n++] = '/';
    }
    put(out, cap, &n, leaf);
    return total;
}

int dk_path_parent(char *dir)
{
    int n = slen(dir);
    if (n == 0)
        return 0;
    while (n > 0 && dir[n - 1] != '/')
        n--;
    dir[n > 0 ? n - 1 : 0] = 0;
    return 1;
}

const char *dk_path_leaf(const char *path)
{
    const char *l = path;
    for (; *path; path++)
        if (*path == '/')
            l = path + 1;
    return l;
}

void dk_text_trunc(char *out, const char *name, int max)
{
    int n = slen(name), i, tail, head, o = 0;
    if (max < 0)
        max = 0;
    if (n <= max) {
        for (i = 0; i <= n; i++)
            out[i] = name[i];
        return;
    }
    if (max < 3) {
        for (i = 0; i < max; i++)
            out[o++] = name[i];
        out[o] = 0;
        return;
    }
    tail = max / 2 - 1;
    head = max - 1 - tail;
    for (i = 0; i < head; i++)
        out[o++] = name[i];
    out[o++] = '~';
    for (i = n - tail; i < n; i++)
        out[o++] = name[i];
    out[o] = 0;
}

void dk_text_size(char *out, uint32_t bytes)
{
    uint32_t k = bytes / 1024 + (bytes % 1024 ? 1u : 0u);
    int n;
    if (k <= 999) {
        n = dk_text_utoa(out, k);
        out[n] = 'K';
        out[n + 1] = 0;
    } else {
        uint32_t tenths = (bytes / 52429u + 1) / 2; /* 0.1 MiB, rounded */
        n = dk_text_utoa(out, tenths / 10);
        out[n++] = '.';
        out[n++] = (char)('0' + tenths % 10);
        out[n++] = 'M';
        out[n] = 0;
    }
}

void dk_text_secs(char *out, uint32_t frames, uint32_t rate)
{
    uint32_t tenths = rate ? (frames * 10u + rate / 2) / rate : 0;
    int n;
    if (tenths < 1000) {
        n = dk_text_utoa(out, tenths / 10);
        out[n++] = '.';
        out[n++] = (char)('0' + tenths % 10);
    } else
        n = dk_text_utoa(out, (tenths + 5) / 10);
    out[n++] = 's';
    out[n] = 0;
}

typedef struct err_text {
    int code;
    const char *tail, *info;
} err_text;

static const err_text errs[] = {
    {DK_SAMPLE_E_NOT_FOUND, "!MISSING", "FILE NOT FOUND"},
    {DK_SAMPLE_E_READ, "!READ ERROR", "CARD READ FAILED"},
    {DK_SAMPLE_E_FORMAT, "!BAD FORMAT", "NEEDS PCM16 48K MONO OR STEREO"},
    {DK_SAMPLE_E_TOO_BIG, "!TOO BIG", "DOES NOT FIT THE POOL"},
    {DK_SAMPLE_E_LOST, "!LOST", "BACKING LOST, RELOAD FAILED"},
    {DK_SAMPLE_E_POOL, "!POOL FULL", "POOL FULL: CLEAR AN ENTRY"},
    {DK_SAMPLE_E_CHANGED, "!CHANGED", "FILE CHANGED SINCE SELECTION"},
    {DK_SAMPLE_E_SEL_PATH, "!BAD PATH", "PATH TOO LONG OR BAD"},
    {DK_SAMPLE_E_CONTRACT, "NO SAMPLE POOL", "NO SAMPLE POOL ON THIS DEVICE"},
};

static void err_other(char *out, int code)
{
    int n = 0;
    put(out, 32, &n, "ERR -");
    dk_text_utoa(out + n, (uint32_t)(code < 0 ? -code : code));
}

void dk_sample_pool_err_tail(char *out, int code)
{
    unsigned i;
    int n = 0;
    out[0] = 0;
    for (i = 0; i < sizeof errs / sizeof errs[0]; i++)
        if (errs[i].code == code) {
            put(out, 20, &n, errs[i].tail);
            return;
        }
    put(out, 20, &n, "!");
    err_other(out + 1, code);
}

void dk_sample_pool_err_info(char *out, int code)
{
    unsigned i;
    int n = 0;
    out[0] = 0;
    for (i = 0; i < sizeof errs / sizeof errs[0]; i++)
        if (errs[i].code == code) {
            put(out, 32, &n, errs[i].info);
            return;
        }
    err_other(out, code);
}

void dk_sample_pool_row_text(char *out, int index, int current, int status, const char *name,
                             uint32_t frames, uint32_t rate, uint32_t channels)
{
    char tail[20], nm[DK_CHOOSER_TEXT + 1], secs[8];
    int n = 0, tn = 0, room, code = status < 0 ? status : 0;
    tail[0] = 0;
    if (code)
        dk_sample_pool_err_tail(tail, code);
    else if (status == DK_SAMPLE_QUEUED)
        put(tail, 20, &tn, "QUEUED");
    else if (status == DK_SAMPLE_LOADING)
        put(tail, 20, &tn, "LOADING...");
    else if (status == DK_SAMPLE_READY) {
        dk_text_secs(secs, frames, rate);
        put(tail, 20, &tn, channels == 2 ? "S " : "M ");
        put(tail, 20, &tn, secs);
    }
    out[0] = (char)('0' + index / 10 % 10);
    out[1] = (char)('0' + index % 10);
    out[2] = current ? '>' : ' ';
    out[3] = 0;
    n = 3;
    if (status == DK_SAMPLE_UNUSED) {
        put(out, DK_CHOOSER_TEXT + 1, &n, "-");
        return;
    }
    room = DK_CHOOSER_TEXT - 3 - slen(tail) - 1;
    if (name[0] && room >= 3) {
        dk_text_trunc(nm, name, room);
        put(out, DK_CHOOSER_TEXT + 1, &n, nm);
        put(out, DK_CHOOSER_TEXT + 1, &n, " ");
    }
    put(out, DK_CHOOSER_TEXT + 1, &n, tail);
}
