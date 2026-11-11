/* Checks core/text.h: length, bounded copy, equality, append, concatenation, number text, and that no function writes outside its buffer. */
#include <assert.h>
#include <dk/core/text.h>
#include <string.h>

#define GUARD 0x5a

/* A buffer of `cap` bytes followed by guard bytes; the guard must stay untouched. */
static void fill(char *b, uint32_t cap)
{
    memset(b, GUARD, cap + 4);
}
static void guard_ok(const char *b, uint32_t cap)
{
    for (uint32_t i = 0; i < 4; i++)
        assert(b[cap + i] == GUARD);
}

static void test_len_equal(void)
{
    assert(dk_text_len("") == 0);
    assert(dk_text_len("abc") == 3);
    assert(dk_text_equal("", ""));
    assert(dk_text_equal("abc", "abc"));
    assert(!dk_text_equal("abc", "abd"));
    assert(!dk_text_equal("abc", "ab"));
    assert(!dk_text_equal("ab", "abc"));
    assert(!dk_text_equal("", "a"));
}

static void test_copy(void)
{
    char b[16];
    fill(b, 6);
    dk_text_copy(b, 6, "abcde"); /* exact capacity: 5 bytes and the NUL */
    assert(strcmp(b, "abcde") == 0);
    guard_ok(b, 6);
    fill(b, 6);
    dk_text_copy(b, 6, "abcdefgh"); /* truncated */
    assert(strcmp(b, "abcde") == 0);
    guard_ok(b, 6);
    fill(b, 6);
    dk_text_copy(b, 6, "");
    assert(b[0] == 0);
    fill(b, 1);
    dk_text_copy(b, 1, "abc"); /* room for the NUL only */
    assert(b[0] == 0);
    guard_ok(b, 1);
    fill(b, 0);
    dk_text_copy(b, 0, "abc"); /* no room: nothing is written */
    assert(b[0] == GUARD);
}

static void test_append_cat(void)
{
    char b[16];
    uint32_t n;
    fill(b, 8);
    b[0] = 0;
    n = dk_text_append(b, 8, 0, "ab");
    assert(n == 2);
    n = dk_text_append(b, 8, n, "cde");
    assert(n == 5 && strcmp(b, "abcde") == 0);
    n = dk_text_append(b, 8, n, "fghij"); /* cut at cap - 1 */
    assert(n == 7 && strcmp(b, "abcdefg") == 0);
    n = dk_text_append(b, 8, n, "x"); /* full: unchanged */
    assert(n == 7 && strcmp(b, "abcdefg") == 0);
    guard_ok(b, 8);
    assert(dk_text_append(b, 0, 0, "x") == 0);

    fill(b, 8);
    dk_text_cat(b, 8, "ab", "cd");
    assert(strcmp(b, "abcd") == 0);
    dk_text_cat(b, 8, b, "efghij"); /* out is a */
    assert(strcmp(b, "abcdefg") == 0);
    fill(b, 4);
    dk_text_cat(b, 4, "abcdef", "gh"); /* a alone fills it */
    assert(strcmp(b, "abc") == 0);
    guard_ok(b, 4);
    fill(b, 4);
    strcpy(b, "abcdef"); /* out is a and longer than cap: cut */
    dk_text_cat(b, 4, b, "gh");
    assert(strcmp(b, "abc") == 0);
    fill(b, 4);
    dk_text_cat(b, 4, "", "");
    assert(b[0] == 0);
    fill(b, 0);
    dk_text_cat(b, 0, "a", "b");
    assert(b[0] == GUARD);
}

static void test_numbers(void)
{
    char b[16];
    fill(b, 12);
    assert(dk_text_utoa(b, 0) == 1 && strcmp(b, "0") == 0);
    assert(dk_text_utoa(b, 9) == 1 && strcmp(b, "9") == 0);
    assert(dk_text_utoa(b, 10) == 2 && strcmp(b, "10") == 0);
    assert(dk_text_utoa(b, 4294967295u) == 10 && strcmp(b, "4294967295") == 0);
    assert(dk_text_itoa(b, 0) == 1 && strcmp(b, "0") == 0);
    assert(dk_text_itoa(b, 7) == 1 && strcmp(b, "7") == 0);
    assert(dk_text_itoa(b, -1) == 2 && strcmp(b, "-1") == 0);
    assert(dk_text_itoa(b, -100) == 4 && strcmp(b, "-100") == 0);
    assert(dk_text_itoa(b, 2147483647) == 10 && strcmp(b, "2147483647") == 0);
    assert(dk_text_itoa(b, (int32_t)-2147483647 - 1) == 11 && strcmp(b, "-2147483648") == 0);
    guard_ok(b, 12);
}

static void trunc_is(const char *name, uint32_t max, const char *want)
{
    char b[32];
    fill(b, max + 1);
    dk_text_trunc(b, name, max);
    assert(strcmp(b, want) == 0);
    guard_ok(b, max + 1);
}

static void test_trunc_size_secs(void)
{
    char b[16];
    trunc_is("KICK", 10, "KICK");
    trunc_is("ABCDEFGHIJ", 10, "ABCDEFGHIJ");
    trunc_is("ABCDEFGHIJK", 10, "ABCDE~HIJK");
    trunc_is("ABCDEFGHIJK", 2, "AB");
    trunc_is("ABCDEFGHIJK", 0, "");
    trunc_is("", 5, "");

    dk_text_size(b, 0);
    assert(strcmp(b, "0K") == 0);
    dk_text_size(b, 1);
    assert(strcmp(b, "1K") == 0);
    dk_text_size(b, 1024);
    assert(strcmp(b, "1K") == 0);
    dk_text_size(b, 1025);
    assert(strcmp(b, "2K") == 0);
    dk_text_size(b, 999u * 1024u);
    assert(strcmp(b, "999K") == 0);
    dk_text_size(b, 1024u * 1024u);
    assert(strcmp(b, "1.0M") == 0);
    dk_text_size(b, 1258291u); /* 1.2 MiB */
    assert(strcmp(b, "1.2M") == 0);

    dk_text_secs(b, 0, 48000);
    assert(strcmp(b, "0.0s") == 0);
    dk_text_secs(b, 57600, 48000);
    assert(strcmp(b, "1.2s") == 0);
    dk_text_secs(b, 48000u * 123u, 48000);
    assert(strcmp(b, "123s") == 0);
    dk_text_secs(b, 100, 0); /* no rate: zero */
    assert(strcmp(b, "0.0s") == 0);
}

int main(void)
{
    test_len_equal();
    test_copy();
    test_append_cat();
    test_numbers();
    test_trunc_size_secs();
    return 0;
}
