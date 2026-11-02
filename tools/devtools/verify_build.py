"""Build the windowless daemons (`digi-emud`) once, as the Nx target `digi-control:daemons` does.

    uv run python -m tools.devtools.verify_build daemons [--device dt2,dn2] [--env-out FILE]

Before this, every python-tests process that drove a daemon asked cargo for the binary itself
(tools/patchkit/pk_harness.py `daemon_binary`): the first build of a new generated core ran inside a
test file, in parallel with the other files and the other steps. The Nx target `digi-control:daemons` runs
this once, before the targets that use a daemon.

For each device that has its ready inputs it takes the daemon from a content-addressed store,
`<git common dir>/emud-cache/<device>/<key>/` (digi-emud, key.txt, depfile.d), shared by every worktree of the
clone. The key is `tools/devtools/emud_key.py key`. On a miss it builds the profile-guided daemon when a PGO
profile exists (`pgo-emud-<device>` variant; the launcher's `pgo-use` is never touched), else the plain daemon;
checks that the key did not change during the build; audits cargo's dependency file against the key; and inserts
the result with one rename. The last 4 used keys per device stay. `DIGI_EMUD_<DEV>=<stored digi-emud>` is written
to --env-out (JSON); a stored file is never rewritten, so the test processes run it and ask cargo for nothing.
`pk_harness.daemon_binary` takes that variable before anything else.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent

from tools.devtools import emud_key, stablebin  # noqa: E402
from tools.patchkit import pk_emud_pgo, pk_harness  # noqa: E402

KEEP = 4  # most recently used keys kept per device in the store


def cargo_target_dir(binary: Path) -> Path:
    """The cargo target dir of `<target>[/<triple>]/release/<bin>`."""
    target = binary.parent.parent
    return target.parent if target.name == pk_emud_pgo.digi_env.TRIPLE else target


def build_one(device: str, syx: Path) -> tuple[Path, Path]:
    """Build the daemon of DEVICE as `daemons` does: (release binary, cargo target dir)."""
    if pk_emud_pgo.native_pgo.current_profile(device) is None:
        plain = pk_harness.daemon_binary(device, syx, pgo=False)
        return plain, cargo_target_dir(plain)
    return pk_emud_pgo.build(device), pk_emud_pgo.target_dir(device)


def store_root() -> Path:
    """`<git common dir>/emud-cache`: shared by every worktree of the clone, never committed."""
    done = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    )  # fmt: skip
    return Path(done.stdout.strip()) / "emud-cache"


def evict(device_dir: Path, keep_key: str) -> None:
    """Keep the KEEP most recently used keys of a device (never KEEP_KEY)."""
    entries = []
    for entry in device_dir.iterdir():
        if entry.name.startswith(".") or not entry.is_dir():
            continue
        try:
            entries.append((entry.joinpath("key.txt").stat().st_mtime, entry))
        except OSError:
            entries.append((0.0, entry))
    entries.sort(key=lambda item: item[0], reverse=True)
    for _mtime, entry in entries[KEEP:]:
        if entry.name != keep_key:
            shutil.rmtree(entry, ignore_errors=True)


def ensure_daemon(device: str, syx: Path, root: Path) -> Path:
    """The stored daemon of DEVICE: restored when its key is in ROOT, else built, audited and inserted."""
    key = emud_key.key(device)
    device_dir = root / device
    final = device_dir / key
    binary = final / "digi-emud"
    if binary.is_file():
        (final / "key.txt").touch()
        print(f"{device} daemon: restored {key[:12]}", flush=True)
        return binary
    built, target = build_one(device, syx)
    inputs = emud_key.inputs(device)
    after = emud_key.key_of(inputs)
    if after != key:
        raise pk_harness.HarnessError(
            f"the inputs of {device} changed during the build ({key} -> {after})"
        )
    depfile = built.with_suffix(".d")
    if not depfile.is_file():
        raise pk_harness.HarnessError(f"cargo wrote no {depfile}")
    missing = emud_key.audit(inputs, depfile.read_text(), target)
    if missing:
        lines = "".join(f"\n  {path}" for path in missing)
        raise pk_harness.HarnessError(
            f"{len(missing)} build inputs of {device} are not in the cache key:{lines}"
        )
    device_dir.mkdir(parents=True, exist_ok=True)
    tmp = device_dir / f".tmp-{os.getpid()}"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir()
    with stablebin.build_lock(target):  # cargo may be rewriting BUILT
        shutil.copyfile(built, tmp / "digi-emud")
    (tmp / "digi-emud").chmod(0o755)
    (tmp / "key.txt").write_text(key + "\n")
    shutil.copyfile(depfile, tmp / "depfile.d")
    try:
        os.rename(tmp, final)
        print(f"{device} daemon: built {key[:12]}", flush=True)
    except OSError:
        if not binary.is_file():
            raise
        shutil.rmtree(
            tmp, ignore_errors=True
        )  # another process inserted this key first
        print(
            f"{device} daemon: restored {key[:12]} (inserted by another process)",
            flush=True,
        )
    evict(device_dir, key)
    return binary


def build_daemons(devices: list[str]) -> dict[str, str]:
    """Provide the daemons of DEVICES from the store; publish each path for downstream consumers."""
    env: dict[str, str] = {}
    root = store_root()
    for device in devices:
        cell = pk_harness.Cell(device, pk_emud_pgo.FIRMWARE[device])
        try:
            syx, _ready = pk_harness.firmware_inputs(cell)
        except pk_harness.HarnessError as exc:
            print(f"SKIP {device}: {exc}", flush=True)
            continue
        env[f"DIGI_EMUD_{device.upper()}"] = str(ensure_daemon(device, syx, root))
    return env


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("what", choices=["daemons"])
    ap.add_argument(
        "--device", default="dt2,dn2", help="comma-separated (default: dt2,dn2)"
    )
    ap.add_argument(
        "--env-out",
        type=Path,
        help="write the pinned DIGI_EMUD_<DEV> variables here (JSON)",
    )
    args = ap.parse_args(argv)
    devices = [d for d in args.device.split(",") if d]
    unknown = sorted(set(devices) - set(pk_emud_pgo.FIRMWARE))
    if unknown:
        ap.error(f"unknown devices: {', '.join(unknown)}")
    try:
        env = build_daemons(devices)
    except pk_harness.HarnessError as exc:
        print(f"verify_build: {exc}", file=sys.stderr)
        return 1
    if args.env_out:
        args.env_out.write_text(json.dumps(env, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
