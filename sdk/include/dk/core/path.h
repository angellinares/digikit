/* <dk/core/path.h>: file path helpers. Paths use '/' and the public limit of 255 bytes. Shared source (sdk/src/core/path.c): the packer
   links it when a module calls one of these functions. */
#ifndef DK_PATH_H
#define DK_PATH_H
#include <stdint.h>

#define DK_PATH_MAX 255 /* longest public path, in bytes without the NUL */

/* dir + "/" + leaf into out of cap bytes; returns the length, or 0 when leaf is empty or the path does not fit in 255 bytes or in cap
   (out is then not written). dir "" is the root: the result is leaf alone. */
uint32_t dk_path_join(char *out, uint32_t cap, const char *dir, const char *leaf);
/* Cuts the last part of dir. 1 when it cut one, 0 at the root. */
int dk_path_parent(char *dir);
/* The part of a path after its last '/'. */
const char *dk_path_leaf(const char *path);
#endif
