#!/usr/bin/env python3
"""The firmware's own view of a +Drive image built from a folder (DT2 1.17, and the module store on DN2 1.12).

    uv run python -m tools.firmware.plusdrive_check [--syx FILE] [--keep DIR] [--dn2]

Builds a `plus_drive/dt2/`-style fixture (nested folders, a plain file
/mods/test.dkm, a directory of 700 entries that needs two content
pages, files of 2 and 10 extents (inline and indirect extent records), a
WAV that becomes a native sample), builds the card image with `plusdrive build`
(`--max-extent-pages 4`), boots the DT2 with it
(packages/emulator/boot/examples/plusdrive_check.rs, about 15 s) and calls the
firmware's mount flag, directory iterator, name lookup and file open, size and
read. The listings, sizes and SHA-256 sums must equal the folder's. The fixture's
`digikit/` files go to the module store (the device's `[card.store]` area, not to the path
filesystem); the firmware's block read must return the store
header, the table and each file's bytes. With --dn2 a Digitone II 1.12 boots a
card that holds only `digikit/` files and the firmware's block read
is checked. Exit status 0 only when everything matches.

The former Python emulator ran the same functions on a snapshot
(REF:unicorn-reference-final:tools/plusdrive_check.py); this one boots the
native machine and calls them with `Emulator::call_function`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import random
import shutil
import struct
import subprocess
import sys
import tempfile
import tomllib
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools import python_env  # noqa: E402
from tools.emulator import digi_env, native_target  # noqa: E402
from tools.firmware import config  # noqa: E402
from tools.patchkit import pk_device  # noqa: E402

MANY = 700


def block_read(syx: pathlib.Path) -> int:
    """The firmware's block read (sector, bytes, buffer): anchor cf.storage.fn.block.read of the image's port file."""
    image = config.image_key(syx)
    port = tomllib.loads((ROOT / "devices" / "ports" / f"{image}.toml").read_text())
    return next(
        a["addr"] for a in port["anchor"] if a["name"] == "cf.storage.fn.block.read"
    )


def write_wav(path: pathlib.Path) -> None:
    import wave

    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(48000)
        w.writeframes(
            struct.pack("<2400h", *[(i * 37) % 6000 - 3000 for i in range(2400)])
        )


SELECTION = [
    "modules/test.dkm",
    "big.bin",
]  # digikit/selection.txt of the fixture: the runtime's selection record


def expected_selection(paths: list[str]) -> bytes:
    """The record the builder writes for `paths` (docs/design/digikit-store.md; seq 1, counter 0, no flags)."""
    rec = bytearray(512)
    rec[0:24] = struct.pack(">6I", 0x444B534C, 1, 1, 0, 0, len(paths))
    for i, p in enumerate(paths):
        rec[32 + 56 * i : 32 + 56 * i + len(p)] = p.encode()
    rec[508:512] = struct.pack(">I", zlib.crc32(bytes(rec[:508])))
    return bytes(rec)


def make_store_fixture(folder: pathlib.Path) -> dict[str, bytes]:
    """Write the `digikit/` files (they go to the module store) and return their bytes by store path."""
    rng = random.Random(17)
    store = {
        "modules/test.dkm": rng.randbytes(3000),
        "big.bin": rng.randbytes(70_000),
    }
    for path, data in store.items():
        target = folder / "digikit" / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    (folder / "digikit" / "selection.txt").write_text(
        "# fixture\n" + "\n".join(SELECTION) + "\n"
    )
    return store


def make_fixture(folder: pathlib.Path) -> dict[str, bytes]:
    """Write the fixture and return the plain files' bytes by path."""
    rng = random.Random(16)
    plain = {
        "mods/test.dkm": rng.randbytes(3000),
        "ext/two.bin": rng.randbytes(5 * 0x8000 - 7),
        "ext/ten.bin": rng.randbytes(37 * 0x8000 + 11),
        "top.txt": b"hello +Drive\n",
    }
    for i in range(MANY):
        plain[f"many/entry-{i:04d}-{'x' * 50}.txt"] = f"{i}\n".encode()
    for path, data in plain.items():
        target = folder / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    (folder / "kicks").mkdir()
    write_wav(folder / "kicks" / "tick.wav")
    return plain


def cargo(args: list[str], env: dict[str, str]) -> None:
    done = subprocess.run(["cargo", *args], cwd=ROOT, env=env)
    if done.returncode:
        raise SystemExit(done.returncode)


def plusdrive_binary() -> pathlib.Path:
    """Where build_plusdrive puts the plusdrive CLI."""
    return pathlib.Path(native_target.target_dir("")) / "release" / "plusdrive"


def build_plusdrive() -> pathlib.Path:
    """Build the plusdrive CLI and return its binary."""
    target = native_target.target_dir("")
    cargo(
        [
            "build", "--release", "--locked", "--offline", "--target-dir", target,
            "-p", "plusdrive-format", "--bin", "plusdrive",
        ],
        digi_env.rust_env(),
    )  # fmt: skip
    return plusdrive_binary()


def store_listing(
    plusdrive: pathlib.Path, image: pathlib.Path, device: str
) -> dict[str, tuple[int, int, str]]:
    """`plusdrive store ls`: path -> (first sector relative to the store, bytes, sha256)."""
    done = subprocess.run(
        [str(plusdrive), "store", "ls", str(image), "--device", device],
        cwd=ROOT, env=python_env(), capture_output=True, text=True, check=True,
    )  # fmt: skip
    rows = {}
    for line in done.stdout.splitlines():
        if line.startswith("#") or line.endswith("\tfolder"):
            continue
        path, first, size, sha = line.split("\t")
        rows[path] = (int(first), int(size), sha)
    return rows


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--syx", type=pathlib.Path, help="DT2 .syx (default: the repo's)")
    ap.add_argument(
        "--keep",
        type=pathlib.Path,
        help="build the fixture and image here and keep them",
    )
    ap.add_argument(
        "--dn2",
        action="store_true",
        help="check the module store on a Digitone II boot instead (--syx: its .syx)",
    )
    ap.add_argument(
        "--build-only",
        action="store_true",
        help="build the plusdrive CLI, print its path and exit",
    )
    args = ap.parse_args(argv)
    if args.build_only:
        print(build_plusdrive())
        return 0
    device = "dn2" if args.dn2 else "dt2"
    area = pk_device.store_of(
        device
    )  # devices/*.toml [card.store]: the area differs per device
    STORE_FIRST = area.first
    SELECTION_SECTORS = (
        area.sel_a,
        area.sel_b,
    )  # copy A and B, the last two sectors of the area
    # The DT2 probe's filesystem addresses (plusdrive_check.rs) are DT2 1.17; the block read comes from the port file.
    syx = args.syx or digi_env.firmware_for(device)
    if digi_env.device_for_syx(syx) != device:
        raise SystemExit(f"{syx} is not a known {device.upper()} firmware")
    plusdrive = build_plusdrive()
    release = plusdrive.parent
    cargo(
        [
            "build", "--release", "--locked", "--offline", "--target-dir",
            native_target.target_dir(""),
            "-p", "elektron-native-boot", "--example", "plusdrive_check",
        ],
        digi_env.rust_env(),
    )  # fmt: skip
    work = args.keep or pathlib.Path(tempfile.mkdtemp(prefix="plusdrive-check-"))
    work.mkdir(parents=True, exist_ok=True)
    try:
        folder = work / "plus_drive"
        shutil.rmtree(folder, ignore_errors=True)
        folder.mkdir()
        plain = {} if args.dn2 else make_fixture(folder)
        store = make_store_fixture(folder)
        image = work / f"{device}.img"
        image.unlink(missing_ok=True)
        build = [
            str(release / "plusdrive"), "build", str(folder), "-o", str(image),
            "--device", device, "--syx", str(syx), "--max-extent-pages", "4",
        ]  # fmt: skip
        subprocess.run(build, cwd=ROOT, env=python_env(), check=True)
        listing = store_listing(release / "plusdrive", image, device)
        expect_store = {
            path: (hashlib.sha256(data).hexdigest(), len(data))
            for path, data in store.items()
        }
        got_store = {path: (row[2], row[1]) for path, row in listing.items()}
        ops = [] if args.dn2 else [
            "mounted", "ls:/", "ls:/mods", "ls:/ext", "ls:/many",
            "ls:/kicks", "lookup:2:digikit", "read:/kicks/tick",
            *(f"read:/{p}" for p in ("mods/test.dkm", "ext/two.bin", "ext/ten.bin", "top.txt")),
            f"read:/many/entry-0000-{'x' * 50}.txt", f"read:/many/entry-{MANY - 1:04d}-{'x' * 50}.txt",
        ]  # fmt: skip
        # The module store through the firmware's block read: the header sector, the table, each file.
        ops += [f"block:{STORE_FIRST:x}:512", f"block:{STORE_FIRST + 1:x}:512"]
        ops += [f"block:{sector:x}:512" for sector in SELECTION_SECTORS]
        ops += [
            f"block:{STORE_FIRST + first:x}:{size}"
            for first, size, _ in listing.values()
        ]
        example = [str(release / "examples" / "plusdrive_check")]
        example += [
            "--block-read",
            f"{block_read(syx):x}",
            str(syx),
            str(image),
            *ops,
        ]
        done = subprocess.run(
            example, cwd=ROOT, env=python_env(), capture_output=True, text=True,
        )  # fmt: skip
        results = {}
        for line in done.stdout.splitlines():
            row = json.loads(line)
            results[row["op"]] = row
        if done.returncode and "boot" not in results:
            print(done.stdout + done.stderr)
            return 1
        failures = judge(results, plain)
        failures += judge_store(
            results, image, listing, store, expect_store == got_store, area
        )
        for op in ops:
            row = results.get(op, {})
            print(f"{op[:70]:72} {summary(row)}")
        if failures:
            print("\nFAIL")
            for f in failures:
                print("  " + f)
            return 1
        print(
            f"\nOK: {len(ops)} firmware calls match the folder (boot icount {results['boot']['icount']:,})"
        )
        return 0
    finally:
        if args.keep is None:
            shutil.rmtree(work, ignore_errors=True)


def judge_store(results, image, listing, store, listing_ok, area) -> list[str]:
    STORE_FIRST = area.first
    SELECTION_SECTORS = (area.sel_a, area.sel_b)
    """The firmware's block read returns the store's header, table and files, equal to the image file and the folder."""
    bad = []
    if not listing_ok:
        bad.append(f"plusdrive store ls differs from the folder: {listing}")
    with image.open("rb") as f:

        def region(sector: int, size: int) -> bytes:
            f.seek(sector * 512)
            return f.read(size)

        head = results.get(f"block:{STORE_FIRST:x}:512", {})
        if not head.get("head", "").startswith(b"DKST".hex()):
            bad.append(f"store header: firmware read {head.get('head')!r}, want DKST")
        for op in [o for o in results if o.startswith("block:")]:
            _, sector, size = op.split(":")
            row = results[op]
            want = hashlib.sha256(region(int(sector, 16), int(size))).hexdigest()
            if row.get("sha256") != want or row.get("read") != int(size):
                bad.append(f"{op}: firmware bytes differ from the image file")
    want_rec = hashlib.sha256(expected_selection(SELECTION)).hexdigest()
    for sector in SELECTION_SECTORS:
        row = results.get(f"block:{sector:x}:512", {})
        if row.get("sha256") != want_rec:
            bad.append(
                f"selection record at sector {sector:#x}: firmware read {row.get('sha256')}, want {want_rec}"
            )
    if "selection.txt" in listing:
        bad.append(
            "selection.txt went into the module store; it is the selection record"
        )
    for path, (first, size, _) in listing.items():
        row = results.get(f"block:{STORE_FIRST + first:x}:{size}", {})
        if row.get("sha256") != hashlib.sha256(store[path]).hexdigest():
            bad.append(f"digikit/{path}: firmware bytes differ from the folder's")
    return bad


def summary(row: dict) -> str:
    if "entries" in row:
        return f"{len(row['entries'])} entries"
    if "sha256" in row:
        return f"{row['read']} bytes sha256 {row['sha256'][:12]}"
    if "found" in row:
        return f"found={row['found']} id={row['id']}"
    return str(row.get("value", row.get("error", "?")))


def names(row: dict) -> list[str]:
    return sorted(e["name"] for e in row.get("entries", []))


def judge(results: dict, plain: dict[str, bytes]) -> list[str]:
    bad = []

    def expect(label: str, got, want) -> None:
        if got != want:
            bad.append(f"{label}: got {got!r}, want {want!r}")

    for op, row in results.items():
        if row.get("error"):
            bad.append(f"{op}: {row['error']}")
    if not plain:
        return bad
    expect("mount flag", results.get("mounted", {}).get("value"), 1)
    root = names(results.get("ls:/", {}))
    expect(
        "ls /",
        root,
        sorted([".", "..", "ext", "factory", "kicks", "many", "mods", "top.txt"]),
    )
    expect("ls /mods", names(results.get("ls:/mods", {})), [".", "..", "test.dkm"])
    expect(
        "ls /ext", names(results.get("ls:/ext", {})), [".", "..", "ten.bin", "two.bin"]
    )
    expect(
        "ls /kicks (a .wav is named without the extension)",
        names(results.get("ls:/kicks", {})),
        [".", "..", "tick"],
    )
    many = sorted(
        [".", "..", *(p.split("/", 1)[1] for p in plain if p.startswith("many/"))]
    )
    expect("ls /many (two content pages)", names(results.get("ls:/many", {})), many)
    # digikit/ is in the module store, not in the path filesystem
    expect("lookup digikit", results.get("lookup:2:digikit", {}).get("found"), 0)
    for path, data in plain.items():
        row = results.get(f"read:/{path}")
        if row is None:
            continue
        expect(f"size {path}", row.get("size"), len(data))
        expect(f"read {path}", row.get("read"), len(data))
        expect(f"sha256 {path}", row.get("sha256"), hashlib.sha256(data).hexdigest())
    tick = results.get("read:/kicks/tick", {})
    expect(
        "native sample size (0x40 header + 2400 frames * 2 + 0x10)",
        tick.get("size"),
        0x40 + 4800 + 0x10,
    )
    return bad


if __name__ == "__main__":
    sys.exit(main())
