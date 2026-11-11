/* Checks core/path.h: join limits and the untouched output on refusal, parent at every depth, and the leaf name. */
#include <assert.h>
#include <dk/core/path.h>
#include <string.h>

static void test_join(void)
{
    char b[300];
    char dir[256], leaf[256];
    memset(b, 'x', sizeof b);
    assert(dk_path_join(b, sizeof b, "", "A") == 1 && strcmp(b, "A") == 0);
    assert(dk_path_join(b, sizeof b, "A/B", "C") == 5 && strcmp(b, "A/B/C") == 0);
    /* exact capacity: 3 bytes and the NUL */
    memset(b, 'x', sizeof b);
    assert(dk_path_join(b, 4, "A", "B") == 3 && strcmp(b, "A/B") == 0 && b[4] == 'x');
    /* one byte short: refused and not written */
    memset(b, 'x', sizeof b);
    assert(dk_path_join(b, 3, "A", "B") == 0 && b[0] == 'x');
    assert(dk_path_join(b, 0, "", "A") == 0 && b[0] == 'x');
    /* an empty leaf is refused */
    assert(dk_path_join(b, sizeof b, "A", "") == 0);
    assert(dk_path_join(b, sizeof b, "", "") == 0);
    /* 255 bytes fit, 256 do not */
    memset(dir, 'd', 200);
    dir[200] = 0;
    memset(leaf, 'f', 54);
    leaf[54] = 0;
    assert(dk_path_join(b, sizeof b, dir, leaf) == DK_PATH_MAX);
    memset(leaf, 'f', 55);
    leaf[55] = 0;
    assert(dk_path_join(b, sizeof b, dir, leaf) == 0);
}

static void parent_is(const char *path, int cut, const char *want)
{
    char b[64];
    strcpy(b, path);
    assert(dk_path_parent(b) == cut);
    assert(strcmp(b, want) == 0);
}

static void test_parent_leaf(void)
{
    parent_is("A/B/C", 1, "A/B");
    parent_is("A/B", 1, "A");
    parent_is("A", 1, "");
    parent_is("", 0, "");
    assert(strcmp(dk_path_leaf("A/B/C.wav"), "C.wav") == 0);
    assert(strcmp(dk_path_leaf("C.wav"), "C.wav") == 0);
    assert(strcmp(dk_path_leaf("A/B/"), "") == 0);
    assert(strcmp(dk_path_leaf(""), "") == 0);
}

int main(void)
{
    test_join();
    test_parent_leaf();
    return 0;
}
