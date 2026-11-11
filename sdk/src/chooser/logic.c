#include <dk/chooser/logic.h>
#include <dk/core/text.h>
#include <dk/sample/pool.h>

static char lower(char c)
{
    return c >= 'A' && c <= 'Z' ? (char)(c + 32) : c;
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
    uint32_t n = dk_text_append(out, 32, 0, "ERR -");
    dk_text_utoa(out + n, (uint32_t)(code < 0 ? -code : code));
}

void dk_sample_pool_err_tail(char *out, int code)
{
    unsigned i;
    out[0] = 0;
    for (i = 0; i < sizeof errs / sizeof errs[0]; i++)
        if (errs[i].code == code) {
            dk_text_append(out, 20, 0, errs[i].tail);
            return;
        }
    dk_text_append(out, 20, 0, "!");
    err_other(out + 1, code);
}

void dk_sample_pool_err_info(char *out, int code)
{
    unsigned i;
    out[0] = 0;
    for (i = 0; i < sizeof errs / sizeof errs[0]; i++)
        if (errs[i].code == code) {
            dk_text_append(out, 32, 0, errs[i].info);
            return;
        }
    err_other(out, code);
}

void dk_sample_pool_row_text(char *out, int index, int current, int status, const char *name,
                             uint32_t frames, uint32_t rate, uint32_t channels)
{
    char tail[20], nm[DK_CHOOSER_TEXT + 1], secs[8];
    uint32_t n = 0, tn = 0;
    int room, code = status < 0 ? status : 0;
    tail[0] = 0;
    if (code)
        dk_sample_pool_err_tail(tail, code);
    else if (status == DK_SAMPLE_QUEUED)
        dk_text_append(tail, 20, 0, "QUEUED");
    else if (status == DK_SAMPLE_LOADING)
        dk_text_append(tail, 20, 0, "LOADING...");
    else if (status == DK_SAMPLE_READY) {
        dk_text_secs(secs, frames, rate);
        tn = dk_text_append(tail, 20, tn, channels == 2 ? "S " : "M ");
        dk_text_append(tail, 20, tn, secs);
    }
    out[0] = (char)('0' + index / 10 % 10);
    out[1] = (char)('0' + index % 10);
    out[2] = current ? '>' : ' ';
    out[3] = 0;
    n = 3;
    if (status == DK_SAMPLE_UNUSED) {
        dk_text_append(out, DK_CHOOSER_TEXT + 1, n, "-");
        return;
    }
    room = DK_CHOOSER_TEXT - 3 - (int)dk_text_len(tail) - 1;
    if (name[0] && room >= 3) {
        dk_text_trunc(nm, name, (uint32_t)room);
        n = dk_text_append(out, DK_CHOOSER_TEXT + 1, n, nm);
        n = dk_text_append(out, DK_CHOOSER_TEXT + 1, n, " ");
    }
    dk_text_append(out, DK_CHOOSER_TEXT + 1, n, tail);
}
