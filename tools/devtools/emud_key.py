"""Cache key and input audit of the coupled PGO `digi-emud` daemon (the Nx `daemon-<dev>` targets).

    uv run python -m tools.devtools.emud_key key   --device dt2
    uv run python -m tools.devtools.emud_key audit --device dt2 --depfile FILE [--target-dir DIR]

`key` prints a sha256 over everything the daemon build reads: the source files (SOURCE_GLOBS, content
hashes), the build recipe scripts (RECIPE_FILES), every file of the generated core directory (and of
SHARC_GEN_DIR_2), the PGO profile (content), `rustc -vV` of the pinned toolchain and the build flags.
It resolves the generated core and the profile with the same calls as tools/patchkit/pk_emud_pgo.py.

`audit` reads the dependency file cargo wrote (`digi-emud.d`) and fails when it lists a file that the key does
not cover. Files under the cargo target directory (build-script OUT_DIR files, the binary) are build outputs and
allowed. Registry crates are not listed by cargo; Cargo.lock (in SOURCE_GLOBS) covers them.

The source list is fixed here (cargo metadata was not used); a crate or data file missing from it is found
by the audit, which makes the build fail instead of caching with an incomplete key.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

# git pathspecs (glob magic) of the repository files cargo reads for digi-emud with coupled-audio.
SOURCE_GLOBS = (
    "Cargo.toml",
    "Cargo.lock",
    "mise.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".cargo/**",
    "packages/emulator/**",
    "packages/dkneeds/**",
    "packages/dksample/**",
    "packages/dkupload/**",
    "packages/patchkit/**",
    "modules/core/runtime/fit/Cargo.toml",
    "modules/core/runtime/fit/build.rs",
    "modules/core/runtime/fit/src/**",
    "modules/core/runtime/sdk/Cargo.toml",
    "modules/core/runtime/sdk/build.rs",
    "modules/core/runtime/sdk/src/**",
    "devices/**",
    "tests/scenarios/dn2.json",
    "sdk/sdk.toml",
    "tools/patchkit/toolchain/**",
    "tools/sharc_dev/sharc_core/seed_tables.py",
)

# The scripts that decide how the build runs (generated core, profile, flags, plain build).
RECIPE_FILES = (
    "tools/devtools/emud_key.py",
    "tools/devtools/verify_build.py",
    "tools/devtools/stablebin.py",
    "tools/emulator/digi_env.py",
    "tools/emulator/native_pgo.py",
    "tools/emulator/native_target.py",
    "tools/emulator/native_emu.py",
    "tools/patchkit/pk_emud_pgo.py",
)

MANIFEST = "packages/emulator/control/Cargo.toml"
FEATURES = "coupled-audio"
BIN = "digi-emud"


@dataclasses.dataclass
class EmudInputs:
    files: dict[str, Path]  # repo-relative name -> file
    gen_dirs: dict[str, Path]  # label -> generated core directory
    profdata: Path | None
    rustc: str
    flags: dict[str, str]


def sha256_file(path: Path) -> str:
    with open(path, "rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def tree_files(folder: Path) -> dict[str, Path]:
    """Every file below FOLDER (relative posix name -> path)."""
    if not folder.is_dir():
        raise FileNotFoundError(f"generated core directory {folder} is missing")
    return {
        p.relative_to(folder).as_posix(): p for p in folder.rglob("*") if p.is_file()
    }


def key_of(inputs: EmudInputs) -> str:
    """sha256 over the canonical serialisation of INPUTS (independent of the order of the files)."""
    record = {
        "version": 1,
        "files": sorted((name, sha256_file(p)) for name, p in inputs.files.items()),
        "gen": {
            label: sorted(
                (name, sha256_file(p)) for name, p in tree_files(folder).items()
            )
            for label, folder in sorted(inputs.gen_dirs.items())
        },
        "profdata": sha256_file(inputs.profdata) if inputs.profdata else None,
        "rustc": inputs.rustc,
        "flags": dict(sorted(inputs.flags.items())),
    }
    text = json.dumps(record, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(text.encode()).hexdigest()


def covered_paths(inputs: EmudInputs) -> set[str]:
    """Resolved paths of every file the key hashes."""
    paths = {str(p.resolve()) for p in inputs.files.values()}
    for folder in inputs.gen_dirs.values():
        paths.update(str(p.resolve()) for p in tree_files(folder).values())
    if inputs.profdata:
        paths.add(str(inputs.profdata.resolve()))
    return paths


def parse_depfile(text: str) -> list[str]:
    """The dependencies of a make-style .d file (`target: dep dep`, `\\ ` escapes a space, `\\` + newline continues)."""
    deps: list[str] = []
    for line in text.replace("\\\r\n", " ").replace("\\\n", " ").splitlines():
        cut = -1
        for i, ch in enumerate(line):
            at_separator = ch == ":" and (i + 1 == len(line) or line[i + 1] in " \t")
            if at_separator and (i == 0 or line[i - 1] != "\\"):
                cut = i
                break
        if cut < 0:
            continue
        token = ""
        i = cut + 1
        rest = line
        while i < len(rest):
            ch = rest[i]
            if ch == "\\" and i + 1 < len(rest) and rest[i + 1] == " ":
                token += " "
                i += 2
                continue
            if ch in " \t":
                if token:
                    deps.append(token)
                token = ""
            else:
                token += ch
            i += 1
        if token:
            deps.append(token)
    return list(dict.fromkeys(deps))


def audit(inputs: EmudInputs, depfile_text: str, target_dir: Path) -> list[str]:
    """The depfile entries that the key does not cover and that are no build output (empty: complete)."""
    covered = covered_paths(inputs)
    out_root = str(target_dir.resolve())
    missing: list[str] = []
    for dep in parse_depfile(depfile_text):
        path = str(Path(dep).resolve())
        if path == out_root or path.startswith(out_root + os.sep):
            continue
        if Path(path).is_dir():
            # A build script's rerun-if-changed on a directory: every file below it is an input.
            missing.extend(
                str(f)
                for f in sorted(Path(path).rglob("*"))
                if f.is_file() and str(f.resolve()) not in covered
            )
        elif path not in covered:
            missing.append(dep)
    return missing


def repo_files(root: Path = ROOT) -> dict[str, Path]:
    """Files of the repository (tracked, or untracked and not ignored) that match SOURCE_GLOBS and RECIPE_FILES."""
    specs = [f":(glob){g}" for g in (*SOURCE_GLOBS, *RECIPE_FILES)]
    done = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", *specs],
        cwd=root, check=True, capture_output=True,
    )  # fmt: skip
    names = sorted(set(done.stdout.decode().split("\0")) - {""})
    return {n: root / n for n in names if (root / n).is_file()}


def rustc_version() -> str:
    from tools.emulator import digi_env

    done = subprocess.run(
        ["rustup", "run", digi_env.TOOLCHAIN, "rustc", "-vV"],
        check=True, capture_output=True, text=True,
    )  # fmt: skip
    return done.stdout


def inputs(device: str, environ: Mapping[str, str] | None = None) -> EmudInputs:
    """The inputs of the daemon build of DEVICE, resolved as tools/patchkit/pk_emud_pgo.py does."""
    from tools.emulator import native_pgo
    from tools.patchkit import pk_emud_pgo

    environ = os.environ if environ is None else environ
    files = repo_files()
    gen_dirs = {"gen": pk_emud_pgo.generated_core(device)}
    flags = {
        "device": device,
        "manifest": MANIFEST,
        "features": FEATURES,
        "triple": pk_emud_pgo.digi_env.TRIPLE,
        "profile": "release",
        "locked": "1",
        "bin": BIN,
    }
    if gen2 := environ.get("SHARC_GEN_DIR_2"):
        path = Path(gen2)
        gen_dirs["gen2"] = path if path.is_absolute() else ROOT / path
    flags["SHARC_GEN_DIR_2"] = environ.get("SHARC_GEN_DIR_2", "")
    flags["SHARC_REGIONBENCH_SOL"] = environ.get("SHARC_REGIONBENCH_SOL", "")
    if sol := environ.get("SHARC_REGIONBENCH_SOL"):
        files["env:SHARC_REGIONBENCH_SOL"] = Path(sol)
    profile = native_pgo.current_profile(device)
    profdata = Path(profile) if profile else None
    if profdata:
        flags["rustflags"] = native_pgo.profile_use_flags(
            f"<profdata sha256 {sha256_file(profdata)}>"
        )
    else:
        flags["rustflags"] = "plain"
    return EmudInputs(files, gen_dirs, profdata, rustc_version(), flags)


def key(device: str) -> str:
    return key_of(inputs(device))


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("key", "audit"):
        sp = sub.add_parser(name)
        sp.add_argument("--device", required=True, choices=("dt2", "dn2"))
    sub.choices["audit"].add_argument("--depfile", type=Path, required=True)
    sub.choices["audit"].add_argument(
        "--target-dir",
        type=Path,
        help="cargo target dir (default: the device's pgo-emud dir)",
    )
    args = ap.parse_args(argv)
    try:
        if args.cmd == "key":
            print(key(args.device))
            return 0
        from tools.patchkit import pk_emud_pgo

        target = args.target_dir or pk_emud_pgo.target_dir(args.device)
        missing = audit(inputs(args.device), args.depfile.read_text(), target)
    except (OSError, subprocess.CalledProcessError, SystemExit) as exc:
        print(f"emud_key: {exc}", file=sys.stderr)
        return 2
    if missing:
        print(f"emud_key: {len(missing)} build inputs are not in the cache key:")
        for path in missing:
            print(f"  {path}")
        return 1
    print(f"emud_key audit {args.device}: ok ({args.depfile})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
