/* <dk/core/text.h>: bounded text helpers for module code, without a C library. Every function that writes takes the size of the buffer
   (NUL included), never writes outside it and leaves the text NUL-terminated, unless a function says otherwise. Shared source
   (sdk/src/core/text.c): the packer links it when a module calls one of these functions. */
#ifndef DK_TEXT_H
#define DK_TEXT_H
#include <stdint.h>

/* Bytes before the NUL. */
uint32_t dk_text_len(const char *s);
/* Copies s into out of cap bytes, cut to cap - 1 bytes; cap 0 writes nothing. */
void dk_text_copy(char *out, uint32_t cap, const char *s);
/* 1 when both texts are the same bytes, else 0. */
int dk_text_equal(const char *a, const char *b);
/* Appends s at out[n] (n is the current length), cut to fit cap; returns the new length. n must be below cap; cap 0 returns 0. */
uint32_t dk_text_append(char *out, uint32_t cap, uint32_t n, const char *s);
/* a then b into out of cap bytes, cut to cap - 1 bytes. out may be a, not b. */
void dk_text_cat(char *out, uint32_t cap, const char *a, const char *b);

/* Decimal of v into out, which holds 11 bytes at least; returns the length. */
uint32_t dk_text_utoa(char *out, uint32_t v);
/* Decimal of v with a leading '-' when negative, into out, which holds 12 bytes at least; returns the length. */
uint32_t dk_text_itoa(char *out, int32_t v);

/* name cut to max characters: its start, '~' and its end (the rule of core/digikit-menu). out holds max + 1 bytes. */
void dk_text_trunc(char *out, const char *name, uint32_t max);
/* "92K" (kilobytes rounded up, to 999K), then "1.2M". out holds 8 bytes. */
void dk_text_size(char *out, uint32_t bytes);
/* "1.2s" below 100 s, then "123s", at most "9999s" (longer sounds show 9999s). Writes 7 bytes at most; out holds 8 bytes. */
void dk_text_secs(char *out, uint32_t frames, uint32_t rate);
#endif
