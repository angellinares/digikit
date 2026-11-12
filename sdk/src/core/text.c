#include <dk/core/text.h>

uint32_t dk_text_len(const char *s)
{
    uint32_t n = 0;
    while (s[n])
        n++;
    return n;
}

void dk_text_copy(char *out, uint32_t cap, const char *s)
{
    uint32_t i = 0;
    if (cap == 0)
        return;
    while (s[i] && i + 1u < cap) {
        out[i] = s[i];
        i++;
    }
    out[i] = 0;
}

int dk_text_equal(const char *a, const char *b)
{
    uint32_t i = 0;
    while (a[i] && a[i] == b[i])
        i++;
    return a[i] == b[i];
}

uint32_t dk_text_append(char *out, uint32_t cap, uint32_t n, const char *s)
{
    if (cap == 0)
        return 0;
    while (*s && n + 1u < cap)
        out[n++] = *s++;
    out[n] = 0;
    return n;
}

void dk_text_cat(char *out, uint32_t cap, const char *a, const char *b)
{
    uint32_t n = 0;
    if (cap == 0)
        return;
    if (out == a) {
        n = dk_text_len(out);
        if (n + 1u > cap)
            n = cap - 1u;
        out[n] = 0;
    } else
        n = dk_text_append(out, cap, 0, a);
    dk_text_append(out, cap, n, b);
}

uint32_t dk_text_utoa(char *out, uint32_t v)
{
    char t[10];
    uint32_t n = 0, i = 0;
    do {
        t[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (n)
        out[i++] = t[--n];
    out[i] = 0;
    return i;
}

uint32_t dk_text_itoa(char *out, int32_t v)
{
    if (v < 0) {
        out[0] = '-';
        return 1u + dk_text_utoa(out + 1, 0u - (uint32_t)v);
    }
    return dk_text_utoa(out, (uint32_t)v);
}

void dk_text_trunc(char *out, const char *name, uint32_t max)
{
    uint32_t n = dk_text_len(name), i, tail, head, o = 0;
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
    uint32_t n;
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
    /* 64-bit: frames * 10 does not fit 32 bits for long sounds. Above 9999 s the text stays "9999s" (at most 6 characters and the NUL). */
    uint64_t tenths = rate ? ((uint64_t)frames * 10u + rate / 2u) / rate : 0;
    uint32_t n;
    if (tenths < 1000) {
        n = dk_text_utoa(out, (uint32_t)(tenths / 10));
        out[n++] = '.';
        out[n++] = (char)('0' + tenths % 10);
    } else {
        uint64_t secs = (tenths + 5) / 10;
        n = dk_text_utoa(out, secs > 9999u ? 9999u : (uint32_t)secs);
    }
    out[n++] = 's';
    out[n] = 0;
}
