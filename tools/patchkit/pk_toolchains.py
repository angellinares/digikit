#!/usr/bin/env python3
"""Fetch and build the patchkit toolchains into out/tools (gitignored, never vendored).

    uv run python -m tools.patchkit.pk_toolchains m68k      # binutils + gcc for m68k-elf, from pinned GNU sources
    uv run python -m tools.patchkit.pk_toolchains selache   # public SHARC+ assembler/compiler at the pinned commit
    uv run python -m tools.patchkit.pk_toolchains codegen-gcc  # Rust for the ColdFire: rustc_codegen_gcc + libgccjit (about 1 h)
    uv run python -m tools.patchkit.pk_toolchains status    # what is installed (JSON)

The ColdFire toolchain installs to out/tools/m68k-elf/ (bin/m68k-elf-gcc, ...). Selache builds in
out/tools/selache/ (target/release/selas, seldump, selcc, ...). Both are plain user-space builds; nothing
is installed globally. Paths are resolved by `m68k_bin()` / `selache_dir()` for the tools and tests, which
skip when a toolchain is absent.

`codegen-gcc` builds Rust for the ColdFire MCF54418 (finding 26, findings 23 S1b): the rust-lang GCC fork at the commit
that rustc_codegen_gcc pins, as libgccjit for `--target=m68k-elf` (macOS patches in tools/patchkit/toolchain/), the
rustc_codegen_gcc backend (one 3-line patch that passes -mcpu to the assembler) and a pinned nightly. It needs the `m68k`
toolchain first (its `as` and `ld`), rustup and gmp/mpfr/libmpc/isl/zstd: from Homebrew on macOS, from the system on
Linux (Debian/Ubuntu: libgmp-dev libmpfr-dev libmpc-dev libisl-dev libzstd-dev). Stages skip when their output exists.
Paths: `codegen_gcc_env()` for the build environment, `rust_target_json()` for the target (data layout `i64:16:64`:
the GCC ColdFire ABI aligns 64-bit integers to 2 bytes).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[2]
# The main tree holds out/ (lanes link to it); resolve through the symlink.
TOOLS = (ROOT / "out" / "tools").resolve()
DL = TOOLS / "dl"

BINUTILS = ("2.47", "https://ftp.gnu.org/gnu/binutils/binutils-2.47.tar.xz")
GCC = ("16.2.0", "https://ftp.gnu.org/gnu/gcc/gcc-16.2.0/gcc-16.2.0.tar.xz")
SHA256 = {
    "binutils-2.47.tar.xz": "154ab23b60070e8f27013c22977f1129425d67d1e8acd6e13010e617811e4cff",
    "gcc-16.2.0.tar.xz": "e6738e29597f733270731aa90600f37ffdc045079dfc27ec7e8192cc81085c3e",
}
M68K_PREFIX = TOOLS / "m68k-elf"
SELACHE_URL = "https://github.com/ElekLabs/selache"
SELACHE_REV = "02d6d785b8e3a125ab237a00bb89503aff835f5d"
SELACHE_DIR = TOOLS / "selache"

# Rust through GCC (finding 23 S1b, finding 26). Every revision is pinned.
CG_DIR = TOOLS / "codegen-gcc"
CG_PATCHES = pathlib.Path(__file__).resolve().parent / "toolchain"
_PINS = tomllib.loads(
    (CG_PATCHES / "pins.toml").read_text()
)  # also read by packages/patchkit/src/build/
RUST_NIGHTLY = _PINS["rust_nightly"]
CG_URL = "https://github.com/rust-lang/rustc_codegen_gcc"
CG_REV = _PINS["cg_rev"]
GCC_JIT_URL = "https://github.com/rust-lang/gcc"
GCC_JIT_REV = _PINS["gcc_jit_rev"]
LIBGCC_CF_NAME = "libgcc-cf54418.a"
RUST_TARGET = CG_PATCHES / "m68k-cf-none-elf-a2.json"


def m68k_bin(name: str = "gcc") -> pathlib.Path | None:
    p = M68K_PREFIX / "bin" / f"m68k-elf-{name}"
    return p if p.exists() else None


def libgcc_cf() -> pathlib.Path | None:
    """The integer/float helper archive built for -mcpu=54418 (build_libgcc_cf), or None."""
    p = M68K_PREFIX / "lib" / LIBGCC_CF_NAME
    return p if p.exists() else None


def selache_dir() -> pathlib.Path | None:
    return (
        SELACHE_DIR if (SELACHE_DIR / "target" / "release" / "selas").exists() else None
    )


DARWIN = sys.platform == "darwin"


def codegen_gcc_backend() -> pathlib.Path:
    lib = "librustc_codegen_gcc.dylib" if DARWIN else "librustc_codegen_gcc.so"
    return CG_DIR / "rustc_codegen_gcc" / "target" / "debug" / lib


def codegen_gcc_ready() -> bool:
    return (
        codegen_gcc_backend().exists()
        and (CG_DIR / "jitlib" / "libgccjit.so").exists()
        and (M68K_PREFIX / "bin" / "m68k-elf-as").exists()
    )


def rust_target_json() -> pathlib.Path:
    return RUST_TARGET


def codegen_gcc_env(extra_rustflags: str = "") -> dict[str, str]:
    """Environment for `cargo +nightly -Zbuild-std ...` with the GCC back end for -mcpu=54418."""
    env = dict(os.environ)
    env.pop("RUSTUP_TOOLCHAIN", None)  # it would override the channel
    env["PATH"] = f"{M68K_PREFIX / 'bin'}:{env['PATH']}"
    # libgccjit's built-in driver holds the absolute --with-as path; COMPILER_PATH finds `as` after a move
    env["COMPILER_PATH"] = str(M68K_PREFIX / "m68k-elf" / "bin")
    env["CG_GCCJIT_LIBRARY_PATH"] = str(CG_DIR / "jitlib" / "libgccjit.so")
    flags = f"-Zcodegen-backend={codegen_gcc_backend()} -Cllvm-args=-mcpu=54418 -Cpanic=abort {extra_rustflags}"
    env["RUSTFLAGS"] = flags.strip()
    return env


def run(
    cmd: list[str], cwd: pathlib.Path | None = None, env: dict[str, str] | None = None
) -> None:
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, cwd=cwd, check=True, env=env)


def fetch(url: str) -> pathlib.Path:
    DL.mkdir(parents=True, exist_ok=True)
    dest = DL / url.rsplit("/", 1)[1]
    if (
        not dest.exists()
        or SHA256.get(dest.name) != hashlib.sha256(dest.read_bytes()).hexdigest()
    ):
        run(["curl", "-fL", "-C", "-", "--retry", "5", "-o", str(dest), url])
    want = SHA256.get(dest.name)
    got = hashlib.sha256(dest.read_bytes()).hexdigest()
    if want and got != want:
        sys.exit(f"{dest.name}: sha256 {got} != pinned {want}")
    if not want:
        print(f"UNPINNED {dest.name} sha256 {got}")
    return dest


def unpack(archive: pathlib.Path) -> pathlib.Path:
    out = TOOLS / "src" / archive.name.removesuffix(".tar.xz")
    if not out.exists():
        (TOOLS / "src").mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive) as t:
            t.extractall(TOOLS / "src", filter="data")
    return out


def brew_prefix(formula: str) -> str:
    """Where a library's headers and libs are: Homebrew's prefix for it, or /usr on a system
    without Homebrew (a Linux distribution's packages install there)."""
    if shutil.which("brew") is None and not DARWIN:
        return "/usr"
    return subprocess.run(
        ["brew", "--prefix", formula], capture_output=True, text=True, check=True
    ).stdout.strip()


def build_m68k() -> None:
    jobs = str(os.cpu_count() or 4)
    env = dict(os.environ)
    deps = {f: brew_prefix(f) for f in ("gmp", "mpfr", "libmpc", "isl", "zstd")}
    # binutils
    b_src = unpack(fetch(BINUTILS[1]))
    b_obj = TOOLS / "build" / "binutils"
    b_obj.mkdir(parents=True, exist_ok=True)
    if not (M68K_PREFIX / "bin" / "m68k-elf-as").exists():
        run(
            [
                str(b_src / "configure"),
                f"--prefix={M68K_PREFIX}",
                "--target=m68k-elf",
                "--disable-nls",
                "--disable-werror",
                "--disable-gdb",
                "--disable-sim",
                "--disable-gprof",
                f"--with-zstd={deps['zstd']}",
            ],
            cwd=b_obj,
            env=env,
        )
        run(["make", "-j", jobs], cwd=b_obj, env=env)
        run(["make", "install"], cwd=b_obj, env=env)
    # gcc (C only, no libc/libgcc multilib: freestanding modules)
    g_src = unpack(fetch(GCC[1]))
    g_obj = TOOLS / "build" / "gcc"
    g_obj.mkdir(parents=True, exist_ok=True)
    env["PATH"] = f"{M68K_PREFIX / 'bin'}:{env['PATH']}"
    if not (M68K_PREFIX / "bin" / "m68k-elf-gcc").exists():
        run(
            [
                str(g_src / "configure"),
                f"--prefix={M68K_PREFIX}",
                "--target=m68k-elf",
                "--enable-languages=c",
                "--disable-nls",
                "--disable-multilib",
                "--disable-shared",
                "--disable-threads",
                "--disable-libssp",
                "--disable-libgomp",
                "--disable-libquadmath",
                "--disable-libatomic",
                "--without-headers",
                "--with-newlib",
                "--disable-bootstrap",
                f"--with-gmp={deps['gmp']}",
                f"--with-mpfr={deps['mpfr']}",
                f"--with-mpc={deps['libmpc']}",
                f"--with-isl={deps['isl']}",
                f"--with-zstd={deps['zstd']}",
            ],
            cwd=g_obj,
            env=env,
        )
        run(["make", "-j", jobs, "all-gcc", "all-target-libgcc"], cwd=g_obj, env=env)
        run(["make", "install-gcc", "install-target-libgcc"], cwd=g_obj, env=env)
    if not (M68K_PREFIX / "lib" / LIBGCC_CF_NAME).exists():
        build_libgcc_cf()


def build_libgcc_cf() -> None:
    """libgcc rebuilt for -mcpu=54418 (ColdFire V4e, ISA_C + EMAC) from the same GCC sources and build tree as build_m68k.

    The m68k-elf gcc is configured without multilib, so its libgcc.a is 68000 code (`mulsi3`, `divsi3` and the soft-float
    helpers use forms a ColdFire traps on: tools/coldfire/isacheck.py refuses it). The libgcc Makefile of the existing
    build tree is copied next to the original and `make` runs it with `-mcpu=54418` (the object files are not shared; the
    copy builds in under a minute). The unwind members are dropped (modules use no exceptions; they hold float-register
    forms). The result is out/tools/m68k-elf/lib/libgcc-cf54418.a; `isacheck` must pass over it."""
    gobj = TOOLS / "build" / "gcc" / "m68k-elf"
    src = gobj / "libgcc"
    if not (src / "Makefile").exists():
        sys.exit("build the m68k toolchain first: pk_toolchains m68k")
    dst = gobj / "libgcc-cf"  # a sibling: the Makefile finds ../.././gcc relative to it
    if dst.exists():
        shutil.rmtree(dst)
    dst.mkdir()
    keep = subprocess.run(
        ["tar", "cf", "-", "--exclude=*.o", "--exclude=*.a", "--exclude=*.dep", "."],
        cwd=src,
        capture_output=True,
        check=True,
    ).stdout
    subprocess.run(["tar", "xf", "-"], cwd=dst, input=keep, check=True)
    cc = next(
        ln.split("=", 1)[1].strip()
        for ln in (dst / "Makefile").read_text().splitlines()
        if ln.startswith("CC =")
    )
    env = dict(os.environ)
    env["PATH"] = f"{M68K_PREFIX / 'bin'}:{env['PATH']}"
    run(
        ["make", "-j", str(os.cpu_count() or 4), f"CC={cc} -mcpu=54418", "libgcc.a"],
        cwd=dst,
        env=env,
    )
    out = M68K_PREFIX / "lib" / LIBGCC_CF_NAME
    shutil.copy(dst / "libgcc.a", out)
    members = subprocess.run(
        [str(M68K_PREFIX / "bin" / "m68k-elf-ar"), "t", str(out)],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    drop = [m for m in members if m.startswith("unwind")]
    if drop:
        run([str(M68K_PREFIX / "bin" / "m68k-elf-ar"), "d", str(out), *drop])
    run([str(M68K_PREFIX / "bin" / "m68k-elf-ranlib"), str(out)])
    from tools.coldfire import isacheck

    try:
        isacheck.check_file(out)
    except isacheck.IsaError as e:
        out.unlink()
        sys.exit(f"{out}: {e}")


def _git_checkout(path: pathlib.Path, url: str, rev: str) -> None:
    """A one-commit clone at the pinned revision (shallow fetch of the sha)."""
    if (path / ".git").exists():
        head = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=path, capture_output=True, text=True
        ).stdout.strip()
        if head == rev:
            return
        sys.exit(f"{path}: HEAD {head} is not the pinned {rev}")
    path.mkdir(parents=True, exist_ok=True)
    run(["git", "init", "-q"], cwd=path)
    run(["git", "fetch", "--depth", "1", url, rev], cwd=path)
    run(["git", "checkout", "-q", "FETCH_HEAD"], cwd=path)


def _apply_patch(src: pathlib.Path, patch: pathlib.Path) -> None:
    """Apply once: a patch that is already in (reverse applies cleanly) is skipped."""
    if (
        subprocess.run(
            ["git", "apply", "--check", "-R", str(patch)], cwd=src, capture_output=True
        ).returncode
        == 0
    ):
        return
    run(["git", "apply", str(patch)], cwd=src)


def build_codegen_gcc() -> None:
    """Rust for the ColdFire. About 75 minutes the first time on an Apple M-series Mac, almost all of it the
    libgccjit build (S1b measured 75 min including the fixes now in the patches; a scripted run is not timed yet)."""
    if not (M68K_PREFIX / "bin" / "m68k-elf-as").exists():
        sys.exit("build the m68k toolchain first: pk_toolchains m68k")
    jobs = str(min(os.cpu_count() or 4, 8))
    deps = {f: brew_prefix(f) for f in ("gmp", "mpfr", "libmpc", "isl", "zstd")}
    env = dict(os.environ)
    env.pop("RUSTUP_TOOLCHAIN", None)
    env["PATH"] = f"{M68K_PREFIX / 'bin'}:{env['PATH']}"
    # 1. the pinned nightly with the compiler libraries the back end links
    run(
        [
            "rustup", "toolchain", "install", RUST_NIGHTLY, "--profile", "minimal",
            "-c", "rust-src", "-c", "rustc-dev", "-c", "llvm-tools-preview",
        ],
        env=env,
    )  # fmt: skip
    # 2. libgccjit: GCC (rust-lang fork) for m68k-elf as a host-shared JIT library
    gsrc, gbuild, jitlib = CG_DIR / "gcc-src", CG_DIR / "gcc-build", CG_DIR / "jitlib"
    if not (jitlib / "libgccjit.so").exists():
        _git_checkout(gsrc, GCC_JIT_URL, GCC_JIT_REV)
        if DARWIN:  # Apple clang only: GCC builds the jit sources as they are
            _apply_patch(gsrc, CG_PATCHES / "libgccjit-darwin.patch")
        gbuild.mkdir(parents=True, exist_ok=True)
        if not (gbuild / "Makefile").exists():
            run(
                [
                    str(gsrc / "configure"),
                    f"--prefix={CG_DIR / 'gcc-install'}",
                    "--target=m68k-elf", "--enable-host-shared", "--enable-languages=jit,c",
                    "--disable-bootstrap", "--enable-checking=release", "--disable-nls",
                    "--disable-multilib", "--disable-shared", "--disable-threads",
                    "--disable-libssp", "--disable-libgomp", "--disable-libquadmath",
                    "--disable-libatomic", "--without-headers", "--with-newlib",
                    f"--with-as={M68K_PREFIX / 'bin' / 'm68k-elf-as'}",
                    f"--with-ld={M68K_PREFIX / 'bin' / 'm68k-elf-ld'}",
                    f"--with-gmp={deps['gmp']}", f"--with-mpfr={deps['mpfr']}",
                    f"--with-mpc={deps['libmpc']}", f"--with-isl={deps['isl']}",
                    f"--with-zstd={deps['zstd']}",
                ],
                cwd=gbuild,
                env=env,
            )  # fmt: skip
        # With Apple clang the jit/*.o objects need libc++ headers that GCC's safe-ctype macros clash with; the
        # force-included headers settle the order. `all-gcc` fails once there; `make -C gcc all` finishes.
        # A GCC host compiler (Linux) needs neither.
        pre = (
            ""
            if not DARWIN
            else " ".join(
                f"-include {h}"
                for h in [
                    "vector",
                    "locale",
                    "string",
                    "map",
                    "set",
                    "algorithm",
                    "sstream",
                ]
            )
        )
        subprocess.run(
            ["make", "-j", jobs, "all-gcc"], cwd=gbuild, env=env
        )  # expected to stop in jit/
        run(
            ["make", "-C", "gcc", "-j", jobs, "all", f"CXXFLAGS=-g -O2 {pre}"],
            cwd=gbuild,
            env=env,
        )
        jitlib.mkdir(exist_ok=True)
        built = gbuild / "gcc" / ("libgccjit.0.dylib" if DARWIN else "libgccjit.so.0")
        if not built.exists():
            sys.exit(f"libgccjit did not build: no {built}")
        names = (
            ("libgccjit.so", "libgccjit.dylib", "libgccjit.0.dylib")
            if DARWIN
            else ("libgccjit.so", "libgccjit.so.0")
        )
        for n in names:
            link = jitlib / n
            link.unlink(missing_ok=True)
            # relative, so out/tools can move
            link.symlink_to(os.path.relpath(built, jitlib))
    # 3. the back end, with the -mcpu patch; the clone sits inside the repository's cargo workspace, so each of
    #    its manifests gets an empty [workspace]
    be = CG_DIR / "rustc_codegen_gcc"
    _git_checkout(be, CG_URL, CG_REV)
    _apply_patch(be, CG_PATCHES / "rustc_codegen_gcc-driver-mcpu.patch")
    for toml in sorted(be.rglob("Cargo.toml")):
        if "target" in toml.parts or "build" in toml.relative_to(be).parts[:1]:
            continue
        text = toml.read_text()
        if "[workspace]" not in text:
            toml.write_text(text.rstrip("\n") + "\n\n[workspace]\n")
    (be / "config.toml").write_text(f'gcc-path = "{jitlib}"\n')
    if not codegen_gcc_backend().exists():
        # y.sh builds the back end first, then fails in its own sysroot step (no target for it): the library is
        # all that is needed; the std sysroot comes from -Zbuild-std in the module build.
        subprocess.run(["./y.sh", "build"], cwd=be, env=env)
    if not codegen_gcc_backend().exists():
        sys.exit("rustc_codegen_gcc did not build")


def build_selache() -> None:
    if not (SELACHE_DIR / ".git").exists():
        run(["git", "clone", SELACHE_URL, str(SELACHE_DIR)])
    run(["git", "checkout", "-q", SELACHE_REV], cwd=SELACHE_DIR)
    run(["cargo", "build", "--release"], cwd=SELACHE_DIR)


def status() -> dict[str, object]:
    out: dict[str, object] = {
        "m68k_elf_gcc": None,
        "libgcc_cf54418": str(libgcc_cf()) if libgcc_cf() else None,
        "selache": None,
        "codegen_gcc": (
            {
                "rustc_codegen_gcc": CG_REV,
                "libgccjit_gcc": GCC_JIT_REV,
                "rust": RUST_NIGHTLY,
            }
            if codegen_gcc_ready()
            else None
        ),
    }
    gcc = m68k_bin("gcc")
    if gcc:
        out["m68k_elf_gcc"] = subprocess.run(
            [str(gcc), "--version"], capture_output=True, text=True
        ).stdout.splitlines()[0]
        ld = m68k_bin("ld")
        if ld:
            out["m68k_elf_ld"] = subprocess.run(
                [str(ld), "--version"], capture_output=True, text=True
            ).stdout.splitlines()[0]
    sel = selache_dir()
    if sel:
        rev = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=sel, capture_output=True, text=True
        ).stdout.strip()
        out["selache"] = rev
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument(
        "what", choices=["m68k", "libgcc-cf", "selache", "codegen-gcc", "status"]
    )
    args = ap.parse_args()
    if args.what == "m68k":
        build_m68k()
    elif args.what == "libgcc-cf":
        build_libgcc_cf()
    elif args.what == "selache":
        build_selache()
    elif args.what == "codegen-gcc":
        build_codegen_gcc()
    print(json.dumps(status(), indent=1))


if __name__ == "__main__":
    main()
