#!/usr/bin/env python3
"""Build the shared panel and the desktop emulator once, then run the binary.

With local DN2 ready inputs and a generated core, the bare launcher restores
the audible DN2 session. The private inputs stay out of the repository: every
path is overridable, and unavailable inputs leave the ordinary CF diagnostic
emulator available.

The panel is rebuilt only when its sources changed (DIGI_EMU_PANEL=1 forces
it). The app is built once with cargo build and the binary is run directly;
--build-only stops after the build and prints the binary path. A stale PGO
profile is not used: the build falls back to plain with a warning. `mise run
perf:pgo` retrains it (or launch once with DIGI_EMU_PGO_TRAIN=1).
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools import python_env  # noqa: E402
from tools.emulator import digi_env, emulator_config  # noqa: E402

PANEL_STAMP = ROOT / "out" / "native" / "panel-stamp.json"
PANEL_INPUTS = (
    "apps/desktop/ui",
    "apps/desktop/package.json",
    "apps/web/src",
    "pnpm-lock.yaml",
)


def fast_regions_for(syx: pathlib.Path) -> str:
    """The verified fast regions for this firmware file, or "" if unknown."""
    return digi_env.fast_regions_for(syx)


def set_fast_regions(
    args: list[str], environ: dict[str, str] | os._Environ[str]
) -> emulator_config.Resolution | None:
    """Enable the fast tier for a coupled run unless the user chose otherwise.

    The rules are in `emulator_config.resolve` (explicit values win, DIGI_EMU_FAST=0 adds
    nothing, explicit regions or auto selection suppress fallback roots for that core).
    Returns the resolution of the first core, None when DIGI_EMU_FAST=0.
    """
    if environ.get("DIGI_EMU_FAST") == "0":
        return None
    syx = (
        pathlib.Path(args[args.index("--audio-profile-syx") + 1])
        if "--audio-profile-syx" in args
        else digi_env.firmware(environ)
    )
    # Slot 1's lists are SHARC_FAST_REGIONS and SHARC_BLOCKTRANS; slot 2 reads its own with a _2 suffix, since the
    # addresses belong to one device's DSP image. In a two-core build (`--also-coupled`, whose firmware is the fourth
    # value) slot 1 is always the DN2 core and slot 2 the DT2 core (see `second_core`), so the suffix follows the
    # firmware's device, not the launch order.
    firmwares = [syx]
    if "--also-coupled" in args and (at := args.index("--also-coupled") + 4) < len(
        args
    ):
        firmwares.append(pathlib.Path(args[at]))
    dt2_first = len(firmwares) == 2 and digi_env.device_for_syx(syx) == "dt2"
    suffixes = ["_2", ""] if dt2_first else ["", "_2"]
    first = None
    for suffix, firmware in zip(suffixes, firmwares, strict=False):
        res = digi_env.resolve_tiers(None, firmware, suffix, environ)
        environ.update(res.add)
        first = first or res
    return first


def generated_core(args: list[str] | None = None) -> pathlib.Path:
    """The generated core for a launch: SHARC_GEN_DIR, else the one registered
    for the --dsp-image in ARGS (tools/sharc_dev/aot_gen.py), else the DN2 default."""
    image = None
    if args and "--dsp-image" in args:
        position = args.index("--dsp-image") + 1
        image = args[position] if position < len(args) else None
    return digi_env.generated_dir(image=image)


def pgo_trained_core(device: str) -> pathlib.Path | None:
    """The generated core the current PGO profile of DEVICE was trained on
    (manifest `generated_dir`), so the launcher's default core matches the
    profile without SHARC_GEN_DIR. None unless it exists and is current."""
    try:
        from tools.emulator import native_pgo
    except ImportError:
        return None
    manifest = native_pgo.read_manifest(device)
    value = manifest.get("generated_dir") if manifest else None
    if not isinstance(value, str) or not value:
        return None
    path = pathlib.Path(value)
    if not path.is_dir():
        return None
    try:
        digi_env.check_generated(path)
    except SystemExit:
        return None
    return path


# Launcher options that take a value: the value is not the firmware file (the first positional argument).
VALUE_OPTIONS = {"--card-image", "--card-folder", "--audio-buffer", "--mirror-dir"}


def second_core(
    device: str, *, enabled_by_default: bool = False
) -> tuple[list[str], pathlib.Path | None]:
    """The other device's ready inputs for a binary with both generated cores (`--also-coupled`) and its generated core
    (a preset SHARC_GEN_DIR_2 names the other device's core, as SHARC_GEN_DIR names the launching device's; the caller
    then stores both in slot order). Desktop launches enable it by default; daemons opt in with
    DIGI_EMU_SECOND_CORE=1. DIGI_EMU_SECOND_CORE=0 disables it. The two-core build has its own PGO profile (device
    `both`). Nothing (a one-core launch) when it is off or the other device lacks a ready set or a core."""
    if (
        os.environ.get("DIGI_EMU_SECOND_CORE", "1" if enabled_by_default else "0")
        != "1"
    ):
        return [], None
    other = "dt2" if device == "dn2" else "dn2"
    paths = digi_env.ready_inputs(device=other)
    missing = [label for label, path in paths.items() if not path.is_file()]
    # Not `digi_env.generated_dir`: SHARC_GEN_DIR names the first core and would win over the image's registered one.
    gen = (
        pathlib.Path(os.environ["SHARC_GEN_DIR_2"])
        if os.environ.get("SHARC_GEN_DIR_2")
        else digi_env.gen_for_image(paths["DSP image"])
        or (digi_env.DEFAULT_GENERATED if other == "dn2" else None)
    )
    if missing or gen is None or not gen.is_dir():
        print(
            f"second core: no {other.upper()} inputs (missing {', '.join(missing) or 'generated core'}); one core only",
            file=sys.stderr,
        )
        return [], None
    digi_env.check_generated(gen)
    print(f"second core: {gen} ({other.upper()})", file=sys.stderr)
    return [
        "--also-coupled",
        str(paths["DSP image"]),
        str(paths["DSP state"]),
        str(paths["CF snapshot"]),
        str(paths["firmware"]),
    ], gen


def default_audio_args(
    args: list[str] | None = None, *, second_core_by_default: bool = False
) -> list[str]:
    """Add local ready audio inputs to a normal launcher command line."""
    args = [] if args is None else list(args)
    positional = next(
        (
            value
            for index, value in enumerate(args)
            if not value.startswith("--")
            and (index == 0 or args[index - 1] not in VALUE_OPTIONS)
        ),
        None,
    )
    syx = pathlib.Path(positional) if positional else digi_env.firmware()
    device = digi_env.device_for_syx(syx) or "dn2"
    paths = digi_env.ready_inputs(device=device)
    if positional and device != "dn2":
        paths["firmware"] = syx
    card = (
        pathlib.Path(args[args.index("--card-image") + 1])
        if "--card-image" in args and args.index("--card-image") + 1 < len(args)
        else None
    )
    if device != "dn2" and (card is not None or not paths["CF snapshot"].is_file()):
        paths = ready_for(device, syx, card, paths)
    missing = [label for label, path in paths.items() if not path.is_file()]
    generated = digi_env.generated_dir(image=paths["DSP image"])
    reason = "SHARC_GEN_DIR" if os.environ.get("SHARC_GEN_DIR") else "registered core"
    if device == "dt2" and not os.environ.get("SHARC_GEN_DIR"):
        trained = pgo_trained_core(device)
        if trained is not None:
            generated, reason = trained, "the core the DT2 PGO profile was trained on"
    if not generated.is_dir():
        missing.append("generated DN2 core (set SHARC_GEN_DIR)")
    if missing:
        print(
            "Native audio is not set up (missing "
            + ", ".join(missing)
            + "); launching the CF-only diagnostic emulator. "
            "Use --cf-only to silence this notice.",
            file=sys.stderr,
        )
        return []
    digi_env.check_generated(generated)
    os.environ.setdefault("SHARC_GEN_DIR", str(generated))
    print(f"native core: {generated} ({reason})", file=sys.stderr)
    also, second = second_core(device, enabled_by_default=second_core_by_default)
    if second is not None:
        # Slot order of a two-core build, whichever firmware launches: slot 1 (SHARC_GEN_DIR) is the DN2 core, slot 2
        # (SHARC_GEN_DIR_2) the DT2 core. The engine is chosen by the firmware's device at run time, and the
        # profile, the target dir and the region lists are keyed by slot.
        dn2_core, dt2_core = (
            (second, generated) if device == "dt2" else (generated, second)
        )
        os.environ["SHARC_GEN_DIR"] = str(dn2_core)
        os.environ["SHARC_GEN_DIR_2"] = str(dt2_core)
        args.extend(also)
    if positional is None:
        args.insert(0, str(paths["firmware"]))
    args.extend(
        [
            "--auto-coupled",
            "--audio-profile-syx",
            str(paths["firmware"]),
            "--dsp-image",
            str(paths["DSP image"]),
            "--dsp-state",
            str(paths["DSP state"]),
            "--cf-snapshot",
            str(paths["CF snapshot"]),
        ]
    )
    # 0.3 s of prefill: the paced app held DN2 and DT2 at 0 underruns with it (finding 18 "Desktop app
    # real time"); pass --audio-buffer 1 on a slower machine, or 0 for the lowest latency.
    if "--audio-buffer" not in args:
        args.extend(["--audio-buffer", "0.3"])
    return args


def ready_for(
    device: str,
    syx: pathlib.Path,
    card: pathlib.Path | None,
    paths: dict[str, pathlib.Path],
) -> dict[str, pathlib.Path]:
    """The ready inputs of a device whose snapshot depends on the card (DT2:
    the sample list is in the restored RAM), built from the .syx alone by
    tools/emulator/ready.py on first use and reused after."""
    command = [
        sys.executable,
        "-m",
        "tools.emulator.ready",
        device,
        "--syx",
        str(syx),
    ]
    if card is not None:
        command += ["--card-image", str(card)]
    print(
        f"Preparing the {device.upper()} audio state for this card (first time only)...",
        file=sys.stderr,
    )
    done = subprocess.run(
        command, cwd=ROOT, env=python_env(), capture_output=True, text=True
    )
    sys.stderr.write(done.stderr[-2000:])
    ready_dir = next(
        (
            line.partition("=")[2]
            for line in done.stdout.splitlines()
            if line.startswith("ready_dir=")
        ),
        None,
    )
    if done.returncode or ready_dir is None:
        print(
            f"tools/emulator/ready.py {device} failed:\n{done.stdout[-2000:]}",
            file=sys.stderr,
        )
        return paths
    base = pathlib.Path(ready_dir)
    return {
        **paths,
        "DSP image": base / "digi-audio-dn2-image.bin",
        "DSP state": base / "digi-audio-m5.snap.dsp",
        "CF snapshot": base / "digi-audio-m5.snap",
    }


def target_dir_for(gen: pathlib.Path | None, second: str | None = None) -> str:
    """The shared cargo target dir for a generated core (tools/emulator/native_target.py); a second core (SHARC_GEN_DIR_2) is part of the key."""
    from tools.emulator import native_target

    # "" means no generated core, whatever SHARC_GEN_DIR says.
    return str(native_target.target_dir(gen if gen is not None else "", second=second))


def coupled_features() -> str:
    """The cargo feature of a coupled build: `second-core` when SHARC_GEN_DIR_2 names a second generated core."""
    return "second-core" if os.environ.get("SHARC_GEN_DIR_2") else "coupled-audio"


def cargo_command(args: list[str], pgo: object | None = None) -> list[str]:
    """The cargo command line; pgo is a native_pgo.LaunchPlan for the PGO build.

    The target dir is keyed by the generated core, so every build of the same
    core (this app, tests, examples) reuses one compiled copy of it.
    """
    command = [
        "cargo",
        "run",
        "--release",
        "--locked",
        "--manifest-path",
        "apps/desktop/src-tauri/Cargo.toml",
    ]
    if "--coupled" in args or "--auto-coupled" in args:
        command.extend(["--features", coupled_features()])
        if pgo is not None:
            command = [*pgo.prefix, *command, *pgo.cargo_args]  # type: ignore[attr-defined]
        else:
            command.extend(
                [
                    "--target-dir",
                    target_dir_for(
                        generated_core(args), os.environ.get("SHARC_GEN_DIR_2")
                    ),
                ]
            )
    else:
        command.extend(["--target-dir", target_dir_for(None)])
    return [*command, "--", *args]


def device_for_args(args: list[str]) -> str:
    """The PGO profile device of a coupled launch: `both` for a two-core build (SHARC_GEN_DIR_2 is set), else the
    device of its firmware file (`dn2`, `dt2`)."""
    if os.environ.get("SHARC_GEN_DIR_2"):
        return "both"
    if "--audio-profile-syx" in args:
        position = args.index("--audio-profile-syx") + 1
        syx = pathlib.Path(args[position]) if position < len(args) else None
    else:
        syx = digi_env.firmware()
    return (digi_env.device_for_syx(syx) if syx else None) or "dn2"


def train_profile(device: str = "dn2") -> str:
    """Run `native_pgo.py train`; returns "" on success, else why it failed."""
    try:
        done = subprocess.run(
            [
                sys.executable,
                "-m",
                "tools.emulator.native_pgo",
                "train",
                *(["--device", device] if device != "dn2" else []),
            ],
            env=python_env(),
            cwd=ROOT,
            check=False,
        )
    except OSError as exc:
        return f"could not start training: {exc}"
    return f"training failed (exit {done.returncode})" if done.returncode else ""


PGO_REPORT: dict[str, str] = {}


def pgo_env() -> dict[str, str]:
    """DIGI_PGO_STATUS and DIGI_PGO_DETAIL for the child, from the last pgo_plan; empty when unknown."""
    if not PGO_REPORT:
        return {}
    return {
        "DIGI_PGO_STATUS": PGO_REPORT["status"],
        "DIGI_PGO_DETAIL": PGO_REPORT["detail"],
    }


def note_pgo(status: str, detail: str) -> None:
    """Remember the PGO state of this launch (partial counts as ok; an ok detail is a path, so it is dropped)."""
    if status in ("ok", "partial"):
        status, detail = "ok", ""
    PGO_REPORT.update(status=status, detail=detail)


def pgo_plan(args: list[str]) -> object | None:
    """Profile-use plan for the coupled build, or None for the ordinary build.

    Each single-core device has its own profile (`native_pgo.py train --device dt2`): a DN2
    profile gives a DT2 build only the shared-engine 3%, a DT2 profile 19%. The two-core build (SHARC_GEN_DIR_2
    set, the desktop default) has the profile of device `both` (`train --device both`), built in a target dir of its
    own. The launcher never trains on its own. A stale or missing profile falls back
    to the ordinary build (its own target dir, not the launcher's pgo-use). DIGI_EMU_PGO_TRAIN=1 trains a stale or missing
    profile here when the training inputs exist (`mise run perf:pgo` is the usual
    way); DIGI_EMU_PGO=0 skips PGO. Any failure falls back to the ordinary
    build with a WARNING line: launching is never blocked.
    """
    PGO_REPORT.clear()
    if "--coupled" not in args and "--auto-coupled" not in args:
        return None
    try:
        from tools.emulator import native_pgo
    except ImportError:
        return None
    environ = os.environ
    device = device_for_args(args)
    status, detail = native_pgo.pgo_state(environ, device)
    if status == "off":
        return None
    if status in ("stale", "missing"):
        if environ.get("DIGI_EMU_PGO_TRAIN") == "1":
            blocker = native_pgo.training_blocker(environ, device)
            if blocker:
                detail = f"{detail}; cannot train: {blocker}"
            else:
                print(
                    f"native PGO: profile {status} ({detail}); training (~6 min, "
                    "DIGI_EMU_PGO_TRAIN=1)",
                    file=sys.stderr,
                    flush=True,
                )
                failure = train_profile(device)
                if failure:
                    status, detail = "failed", failure
                else:
                    status, detail = native_pgo.pgo_state(environ, device)
        elif status == "stale":
            note_pgo(status, detail)
            print(native_pgo.stale_notice(detail, device), file=sys.stderr)
            return None
        else:
            note_pgo(status, detail)
            print(native_pgo.fallback_notice(status, detail, device), file=sys.stderr)
            return None
    if status == "partial":
        profile = native_pgo.current_profile(device)
        if profile:
            note_pgo(status, detail)
            print(native_pgo.PARTIAL_NOTICE, file=sys.stderr)
            return native_pgo.plan_for(profile, environ)
        status, detail = "missing", "profile file is gone"
    if status != "ok":
        note_pgo(status, detail)
        print(native_pgo.fallback_notice(status, detail, device), file=sys.stderr)
        return None
    note_pgo(status, detail)
    print(
        f"native PGO: using {device.upper() + ' ' if device != 'dn2' else ''}profile "
        + pathlib.Path(detail).name,
        file=sys.stderr,
    )
    return native_pgo.plan_for(detail, environ)


PGO_MISMATCH_WARNING = (
    "WARNING: PGO profile does not match this binary (crate hashes differ); "
    "live DSP will run ~35% slower. Run uv run python -m tools.emulator.native_pgo train."
)
PGO_MISMATCH_WARNING_DT2 = PGO_MISMATCH_WARNING.replace("train.", "train --device dt2.")
PGO_MISMATCH_WARNING_BOTH = PGO_MISMATCH_WARNING.replace(
    "train.", "train --device both."
)


def build_command(command: list[str]) -> list[str]:
    """The `cargo run` command as a `cargo build` of the same binary."""
    cut = command.index("--")
    head = command[:cut]
    head[head.index("cargo") + 1] = "build"
    return head


def verify_pgo(command: list[str], env: dict[str, str], device: str = "dn2") -> None:
    """Build, then warn loudly if the profile's crate hashes miss the binary.

    A failed build stops the launch; a failed check only warns.
    """
    built = subprocess.run(build_command(command), cwd=ROOT, env=env)
    if built is not None and built.returncode:
        raise SystemExit(built.returncode)
    try:
        done = subprocess.run(
            [
                sys.executable,
                "-m",
                "tools.emulator.native_pgo",
                "verify",
                "--device",
                device,
                *(
                    ["--binary", str(app_binary(command, True))]
                    if "--target-dir" in command
                    else []
                ),
            ],
            env=python_env(),
            cwd=ROOT,
            check=False,
        )
    except (OSError, ValueError):
        return
    if done.returncode == 1:
        warning = {
            "dt2": PGO_MISMATCH_WARNING_DT2,
            "both": PGO_MISMATCH_WARNING_BOTH,
        }.get(device, PGO_MISMATCH_WARNING)
        print(warning, file=sys.stderr, flush=True)
        note_pgo("mismatch", "profile does not match this build")


def daemon_command(args: list[str]) -> list[str]:
    """`cargo run` for the windowless daemon `digi-emud` (packages/emulator/control).

    Same target-dir rules as the desktop app, so a coupled daemon and the app
    share one compiled copy of the generated core. No PGO: the profile is
    trained on the desktop binary.
    """
    command = [
        "cargo",
        "run",
        "--release",
        "--locked",
        "--manifest-path",
        "packages/emulator/control/Cargo.toml",
        "--bin",
        "digi-emud",
    ]
    if "--coupled" in args or "--auto-coupled" in args:
        command.extend(
            [
                "--features",
                coupled_features(),
                "--target-dir",
                target_dir_for(generated_core(args), os.environ.get("SHARC_GEN_DIR_2")),
            ]
        )
    else:
        command.extend(["--target-dir", target_dir_for(None)])
    return [*command, "--", *args]


def app_binary(command: list[str], plan: object | None) -> pathlib.Path:
    """The binary the build command produces (digiemu-desktop, or digi-emud)."""
    target = pathlib.Path(command[command.index("--target-dir") + 1])
    name = (
        command[command.index("--bin") + 1]
        if "--bin" in command[: command.index("--")]
        else "digiemu-desktop"
    )
    if plan is not None:
        return target / digi_env.TRIPLE / "release" / name
    return target / "release" / name


def panel_digest() -> str:
    """Digest of the panel's sources (the browser WASM core is left out)."""
    outer = hashlib.sha256()
    for rel in PANEL_INPUTS:
        top = ROOT / rel
        if top.is_file():
            files = [top]
        else:
            files = sorted(p for p in top.rglob("*") if p.is_file())
        for path in files:
            if path.name.startswith("emulator-core") and path.suffix == ".wasm":
                continue
            outer.update(f"{path.relative_to(ROOT)}\0".encode())
            outer.update(hashlib.sha256(path.read_bytes()).digest())
    return outer.hexdigest()


def build_panel(environ: dict[str, str] | os._Environ[str]) -> None:
    """Build the desktop console (apps/desktop/ui) unless its sources match the last build.

    DIGI_EMU_PANEL=1 forces the build.
    """
    digest = panel_digest()
    dist = ROOT / "apps" / "desktop" / "ui" / "dist"
    try:
        same = json.loads(PANEL_STAMP.read_text()).get("digest") == digest
    except (OSError, ValueError):
        same = False
    if same and dist.is_dir() and environ.get("DIGI_EMU_PANEL") != "1":
        return
    subprocess.run(
        ["pnpm", "--filter", "@digi/desktop", "run", "build:ui"],
        cwd=ROOT,
        check=True,
    )
    PANEL_STAMP.parent.mkdir(parents=True, exist_ok=True)
    PANEL_STAMP.write_text(json.dumps({"digest": digest}) + "\n")


def plus_drive_files(root: pathlib.Path) -> list[pathlib.Path]:
    """Every regular file under `root` (relative paths, sorted); symbolic links and
    .DS_Store are left out, as the builder leaves them out."""
    found = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        here = pathlib.Path(dirpath)
        for name in sorted(filenames):
            path = here / name
            if name != ".DS_Store" and not path.is_symlink():
                found.append(path.relative_to(root))
    return found


def plusdrive_card(
    folder: pathlib.Path, syx: pathlib.Path, env: dict[str, str], device: str = "dt2"
) -> pathlib.Path:
    """Build (or reuse) a +Drive card image from a `plus_drive/<device>/` folder.

    The folder contract (docs/findings/14-plus-drive-format.md, "plus_drive/"): files and
    folders map to the path filesystem as they are; a `.wav` becomes a native
    sample named without the extension (24 -> 16 bit, 48 kHz); any other file is
    copied byte for byte (a file that is a native sample stays one);
    `.mmcfs/{projects,sounds,kits}/NNNN.bin` are raw MmcFs slots;
    `.mmcfs/boot/SSSSSS.bin` are the project regions outside the pools (a boot
    copy replaces the generated boot project); `digikit/` goes to the module
    store (docs/design/digikit-store.md). DN2 has no path filesystem: only
    `.mmcfs/` and `digikit/` apply. The card layout of the device comes from
    devices/<device>.toml `[card]`.

    Uses the native `plusdrive build` (packages/emulator/plusdrive). The image lives in
    out/plusdrive/native/<key12>/<device>.img, keyed by the SHA-256 of the firmware,
    the plusdrive binary and every file's relative path and bytes, so unchanged
    inputs reuse the image.
    """
    if not folder.is_dir():
        raise SystemExit(f"{folder} is not a directory")
    files = plus_drive_files(folder)
    if not files:
        raise SystemExit(f"{folder} holds no files")
    manifest = ROOT / "packages" / "emulator" / "plusdrive" / "Cargo.toml"
    # Share the CF-only target dir with the desktop app: one dependency build.
    target = target_dir_for(None)
    done = subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "--locked",
            "--manifest-path",
            str(manifest),
            "--bin",
            "plusdrive",
            "--target-dir",
            target,
        ],
        cwd=ROOT,
        env=env,
    )
    if done.returncode:
        raise SystemExit(done.returncode)
    binary = pathlib.Path(target) / "release" / "plusdrive"
    h = hashlib.sha256()
    h.update(b"firmware\0" + hashlib.sha256(syx.read_bytes()).digest())
    h.update(b"plusdrive\0" + hashlib.sha256(binary.read_bytes()).digest())
    h.update(b"device\0" + device.encode() + b"\0")
    for rel in files:
        h.update(b"file\0" + rel.as_posix().encode() + b"\0")
        h.update(hashlib.sha256((folder / rel).read_bytes()).digest())
    key = h.hexdigest()[:12]
    image = ROOT / "out" / "plusdrive" / "native" / key / f"{device}.img"
    if image.is_file():
        print(f"+Drive card: reusing {image} ({len(files)} file(s))", file=sys.stderr)
        return image
    image.parent.mkdir(parents=True, exist_ok=True)
    # One partial file per process: parallel verify steps can build the same
    # key at once, and os.replace lets the last one win atomically.
    partial = image.with_name(f"{device}.{os.getpid()}.part")
    done = subprocess.run(
        [
            str(binary),
            "build",
            str(folder),
            "-o",
            str(partial),
            "--device",
            device,
            "--syx",
            str(syx),
        ],
        cwd=ROOT,
        env=env,
    )
    if done.returncode:
        partial.unlink(missing_ok=True)
        raise SystemExit(done.returncode)
    os.replace(partial, image)
    print(f"+Drive card: built {image} ({len(files)} file(s))", file=sys.stderr)
    return image


def take_option(args: list[str], name: str) -> tuple[str | None, list[str]]:
    """The value of `--name VALUE` and `args` without it."""
    if name not in args:
        return None, args
    i = args.index(name)
    if i + 1 >= len(args):
        raise SystemExit(f"{name} needs a directory")
    return args[i + 1], args[:i] + args[i + 2 :]


def syx_from_args(args: list[str]) -> pathlib.Path:
    """The firmware file the launcher will run: the first positional argument,
    else the default."""
    syx_arg = next(
        (
            v
            for j, v in enumerate(args)
            if not v.startswith("--") and (j == 0 or args[j - 1] not in VALUE_OPTIONS)
        ),
        None,
    )
    return pathlib.Path(syx_arg) if syx_arg else digi_env.firmware(os.environ)


def main(args: list[str] | None = None) -> None:
    args = sys.argv[1:] if args is None else args
    force_cf = "--cf-only" in args
    build_only = "--build-only" in args
    daemon = "--daemon" in args
    no_sync = "--no-sync" in args
    args = [
        arg
        for arg in args
        if arg not in ("--cf-only", "--build-only", "--daemon", "--no-sync")
    ]
    plus_drive, args = take_option(args, "--plus-drive")
    samples, args = take_option(args, "--samples")
    if plus_drive is not None and samples is not None:
        raise SystemExit("--plus-drive and --samples cannot be combined")
    folder = pathlib.Path(plus_drive or samples) if (plus_drive or samples) else None
    if folder is not None and "--card-image" in args:
        raise SystemExit("--plus-drive/--samples and --card-image cannot be combined")
    syx = syx_from_args(args)
    known = digi_env.device_for_syx(syx)
    device = known or "dn2"
    if folder is not None:
        if plus_drive is None and samples is None:
            print(f"+Drive: {folder}", file=sys.stderr)
        env = digi_env.rust_env(dict(os.environ))
        # A patched image gets the card of its stock base (the same card as `inject` keeps).
        card = plusdrive_card(folder, digi_env.base_firmware(syx), env, device)
        first_positional = any(
            not v.startswith("--") and (j == 0 or args[j - 1] not in VALUE_OPTIONS)
            for j, v in enumerate(args)
        )
        args = [
            *([] if first_positional else [str(syx)]),
            *args,
            "--card-image",
            str(card),
            # The app exports what the firmware wrote to the card back into the folder on exit
            # (a log line says how many files); --no-sync leaves the folder alone.
            "--card-folder",
            str(folder.resolve()),
            *(["--no-card-sync"] if no_sync else []),
        ]
        if not no_sync:
            print(
                f"+Drive: changes on the card are exported to {folder} on exit (--no-sync turns it off)",
                file=sys.stderr,
            )
    # One folder per device, chosen by the app (Rust: the controller): plus_drive/<device>/ or the folder the user chose in the Card
    # panel (kept per device in the app's data folder). On every load the controller builds the card from the loaded device's folder
    # when it holds something the card takes, and mirrors the module store to it while the emulator runs (the Card panel: files,
    # "Sync now"). --plus-drive/--samples name the folder of the first load instead (a card built here from it). --no-sync turns
    # the mirror and the export on exit off.
    if known and "--device-folders" not in args:
        args = [
            *args,
            "--device-folders",
            *(["--no-card-sync"] if no_sync and folder is None else []),
        ]
    if folder is not None and not no_sync and "--mirror-dir" not in args:
        args = [*args, "--mirror-dir", str(folder.resolve())]
    if force_cf and ("--coupled" in args or "--auto-coupled" in args):
        raise SystemExit("--cf-only cannot be combined with coupled audio options")
    if not force_cf and "--coupled" not in args and "--auto-coupled" not in args:
        args = default_audio_args(args, second_core_by_default=not daemon)
    fast = None
    if "--coupled" in args or "--auto-coupled" in args:
        generated = generated_core(args)
        if not generated.is_dir():
            raise SystemExit(
                "--coupled needs SHARC_GEN_DIR pointing to a local generated DN2 core"
            )
        digi_env.check_generated(generated)
        os.environ.setdefault("SHARC_GEN_DIR", str(generated))
        fast = set_fast_regions(args, os.environ)
    # The native shell serves the built panel; it does not load browser WASM.
    # Avoid rebuilding the browser core (and its local DSP code) for a desktop run.
    # The daemon has no window: no panel, no PGO.
    if not daemon:
        build_panel(os.environ)
    plan = None if daemon else pgo_plan(args)
    env = digi_env.rust_env({**os.environ, **(plan.env if plan is not None else {})})  # type: ignore[attr-defined]
    command = daemon_command(args) if daemon else cargo_command(args, plan)
    if plan is not None:
        verify_pgo(command, env, device_for_args(args))
    else:
        done = subprocess.run(build_command(command), cwd=ROOT, env=env)
        if done is not None and done.returncode:
            raise SystemExit(done.returncode)
    binary = app_binary(command, plan)
    if build_only:
        print(binary)
        return
    if not daemon:
        env.update(pgo_env())
        # Workbench default from config/emulator.json; an explicit value, including an empty one, still wins.
        context = digi_env.resolve_tiers(
            "workbench", digi_env.firmware(env), "", env, lists=False
        )
        env.update(context.add)
        print(emulator_config.startup_line("workbench", env, fast, context), flush=True)
    subprocess.run([str(binary), *args], cwd=ROOT, check=True, env=env)


if __name__ == "__main__":
    main()
