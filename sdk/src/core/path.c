#include <dk/core/path.h>
#include <dk/core/text.h>

uint32_t dk_path_join(char *out, uint32_t cap, const char *dir, const char *leaf)
{
    uint32_t dl = dk_text_len(dir), ll = dk_text_len(leaf);
    uint32_t total = dl ? dl + 1 + ll : ll;
    uint32_t n = 0;
    if (ll == 0 || total > DK_PATH_MAX || total + 1 > cap)
        return 0;
    if (dl) {
        n = dk_text_append(out, cap, n, dir);
        out[n++] = '/';
    }
    dk_text_append(out, cap, n, leaf);
    return total;
}

int dk_path_parent(char *dir)
{
    uint32_t n = dk_text_len(dir);
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
