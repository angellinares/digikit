/* Declarations and stubs for POSIX process calls that wasi-libc lacks.
   Only the GNU libiberty pex-unix.c driver code references them; the wasm
   tools never spawn processes, so every stub fails with ENOSYS. */
#ifndef WASI_SHIM_H
#define WASI_SHIM_H
#include <errno.h>
#include <sys/types.h>
#ifdef __cplusplus
extern "C" {
#endif
static inline char *mktemp(char *t)
{
    return t;
}
static inline mode_t umask(mode_t m)
{
    (void)m;
    return 022;
}
static inline pid_t fork(void)
{
    errno = ENOSYS;
    return -1;
}
static inline pid_t wait(int *s)
{
    (void)s;
    errno = ENOSYS;
    return -1;
}
static inline int execv(const char *p, char *const a[])
{
    (void)p;
    (void)a;
    errno = ENOSYS;
    return -1;
}
static inline int execvp(const char *p, char *const a[])
{
    (void)p;
    (void)a;
    errno = ENOSYS;
    return -1;
}
static inline int kill(pid_t p, int s)
{
    (void)p;
    (void)s;
    errno = ENOSYS;
    return -1;
}
#ifdef __cplusplus
}
#endif
#endif
