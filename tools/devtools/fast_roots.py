"""Find the generated SHARC regions that the runtime patches bar, and guard the interpreted work.

    uv run python -m tools.devtools.fast_roots rank  --prefix PREFIX --device dn2|dt2 [--gen DIR] [--min-pct 0.3] [--json]
    uv run python -m tools.devtools.fast_roots check --device dn2,dt2 [--seconds 3]
    uv run python -m tools.devtools.fast_roots fixtures --device dn2,dt2

A patched PC bars every entry of its generated region (packages/emulator/sharc/src/code_diff.rs), so the
code of that region runs in the interpreter. `fallback_regions` in config/emulator.json arm
roots that stay fast.

`rank` reads one capture (`SHARC_PROFILE_PREFIX=PREFIX`, files `PREFIX.e*` and `PREFIX.e*.entries`, see
docs/INSTRUMENTATION.md) and the `REGION_ENTRIES` of the generated core. It ranks the regions by interpreted
instructions and suggests roots: the entries with most interpreter run starts, and the DO-loop bodies (an
entry that runs at least 10 times as many interpreted instructions as it has run starts). Check the
suggested roots for exactness before they go into config/emulator.json.

`check` replays a scenario on a fixture of the CURRENT runtime (cached by a content key) and fails when the
interpreted instructions per audio second (and block entries, when the budget has a limit) are over the budget in
tools/emulator/fast_budgets.json. A device lists one case per fixture state: `first-boot` (the state after the first
boot) and `reset` (the state after a guest soft reset, DN2 only).

`fixtures` only builds or restores the fixtures of the same cases, and prints one line for each.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import dataclasses
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Iterable, Mapping

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
BUDGETS = ROOT / "tools" / "emulator" / "fast_budgets.json"

BLOCK_SPAN = 400  # short words from a block entry to the last PC counted into it
LOOP_RATIO = 10  # interpreted instructions per run start that mark a loop body
STARTS_PER_REGION = 3
LOOPS_PER_REGION = 2
RUN_TIMEOUT = 900.0

# Stock firmware of the fixture (same table as packages/test-support/src/inputs.ts).
STOCK_SYX = {"dn2": "Digitone_II_OS1.12.syx", "dt2": "Digitakt_II_OS1.17.syx"}
PROJECT = ROOT / "tools" / "gate" / "fast-roots" / "project.json"


def key_specs() -> list[str]:
    """Git pathspecs of the files in the fixture key: the fixtureKey input of the gate project."""
    inputs = json.loads(PROJECT.read_text())["namedInputs"]["fixtureKey"]
    return [":(glob)" + str(entry).removeprefix("{workspaceRoot}/") for entry in inputs]


def daemon_env() -> dict[str, str]:
    """DIGI_EMUD_<DEV> from dist/daemons.json (written by digi-control:daemons), so a fixture
    boots the Nx-built daemon instead of building its own. Empty when the file is absent."""
    path = ROOT / "dist" / "daemons.json"
    if not path.exists():
        return {}
    return {
        name: value
        for name, value in json.loads(path.read_text()).items()
        if pathlib.Path(value).exists()
    }


# Rank


def parse_region_entries(text: str) -> list[list[int]]:
    """The `REGION_ENTRIES` of a generated image.rs: one list of entry PCs per region (empty regions dropped)."""
    start = text.find("REGION_ENTRIES")
    if start < 0:
        raise ValueError("image.rs has no REGION_ENTRIES")
    body = text[text.index("=", start) : text.index(";", text.index("=", start))]
    regions = []
    for match in re.finditer(r"&\[([^\[\]]*)\]", body):
        pcs = [
            int(x, 0) for x in match.group(1).replace("_", "").split(",") if x.strip()
        ]
        if pcs:
            regions.append(pcs)
    return regions


def parse_counts(text: str, column: int) -> dict[int, int]:
    """`pc ... count` lines (hex pc, decimal count in COLUMN) as pc -> count; blank and `#` lines are skipped."""
    out: dict[int, int] = collections.defaultdict(int)
    for line in text.splitlines():
        fields = line.split()
        if len(fields) <= column or line.lstrip().startswith("#"):
            continue
        out[int(fields[0], 16)] += int(fields[column])
    return dict(out)


def read_capture(prefix: str) -> tuple[dict[int, int], dict[int, int]]:
    """(interpreted instructions per PC, interpreter run starts per PC), summed over the files of PREFIX."""
    path = pathlib.Path(prefix)
    interpreted: dict[int, int] = collections.defaultdict(int)
    starts: dict[int, int] = collections.defaultdict(int)
    found = False
    for file in sorted(path.parent.glob(path.name + ".e*")):
        if file.suffix == ".trans" or file.name.endswith(".trans"):
            continue
        found = True
        if file.name.endswith(".entries"):
            for pc, n in parse_counts(file.read_text(), 1).items():
                starts[pc] += n
        else:
            for pc, n in parse_counts(file.read_text(), 3).items():
                interpreted[pc] += n
    if not found:
        raise SystemExit(
            f"fast_roots: no capture files {prefix}.e* (folder must exist; quit the app normally)"
        )
    return dict(interpreted), dict(starts)


@dataclasses.dataclass
class Region:
    first: int
    last: int
    instructions: int
    pct: float
    starts: list[int]  # entries with most run starts
    loops: list[int]  # loop-body entries

    @property
    def roots(self) -> list[int]:
        return list(dict.fromkeys([*self.starts, *self.loops]))


@dataclasses.dataclass
class Ranking:
    total: int
    outside: int
    regions: list[Region]

    @property
    def outside_pct(self) -> float:
        return 100.0 * self.outside / max(self.total, 1)

    def roots(self) -> list[int]:
        return sorted({pc for r in self.regions for pc in r.roots})


def rank_regions(
    regions: list[list[int]],
    interpreted: Mapping[int, int],
    starts: Mapping[int, int],
    min_pct: float = 0.3,
) -> Ranking:
    """Attribute each interpreted PC to the block (largest entry <= PC within BLOCK_SPAN) and rank the regions."""
    owner = {pc: i for i, entries in enumerate(regions) for pc in entries}
    sorted_entries = sorted(owner)
    per_entry: dict[int, int] = collections.defaultdict(int)
    outside = 0
    for pc, n in interpreted.items():
        at = bisect.bisect_right(sorted_entries, pc) - 1
        if at >= 0 and pc - sorted_entries[at] <= BLOCK_SPAN:
            per_entry[sorted_entries[at]] += n
        else:
            outside += n
    total = sum(interpreted.values())
    per_region: dict[int, int] = collections.defaultdict(int)
    for pc, n in per_entry.items():
        per_region[owner[pc]] += n
    ranked = []
    for index, n in sorted(per_region.items(), key=lambda item: (-item[1], item[0])):
        pct = 100.0 * n / max(total, 1)
        if pct < min_pct:
            continue
        entries = regions[index]
        by_starts = sorted(
            (pc for pc in entries if starts.get(pc, 0) > 0),
            key=lambda pc: (-starts[pc], pc),
        )[:STARTS_PER_REGION]
        loop_bodies = sorted(
            (
                pc
                for pc in entries
                if per_entry.get(pc, 0) > 0
                and per_entry[pc] >= LOOP_RATIO * starts.get(pc, 0)
            ),
            key=lambda pc: (-per_entry[pc], pc),
        )[:LOOPS_PER_REGION]
        ranked.append(
            Region(min(entries), max(entries), n, pct, by_starts, loop_bodies)
        )
    return Ranking(total, outside, ranked)


def format_ranking(ranking: Ranking) -> str:
    lines = [f"{'percent':>8}  {'region':<18}  roots"]
    for r in ranking.regions:
        roots = " ".join(f"0x{pc:x}" for pc in r.roots)
        lines.append(
            f"{r.pct:>7.2f}%  0x{r.first:x}-0x{r.last:x}".ljust(30) + f"  {roots}"
        )
    lines.append(f"interpreted instructions in the capture: {ranking.total}")
    lines.append(
        f"outside generated blocks: {ranking.outside_pct:.2f}% ({ranking.outside})"
    )
    lines.append(
        "Check every root for exactness before it goes into config/emulator.json."
    )
    lines.append("ROOTS " + ",".join(f"0x{pc:x}" for pc in ranking.roots()))
    return "\n".join(lines)


def ranking_json(ranking: Ranking) -> dict[str, object]:
    return {
        "total": ranking.total,
        "outside": ranking.outside,
        "outside_pct": ranking.outside_pct,
        "roots": [f"0x{pc:x}" for pc in ranking.roots()],
        "regions": [
            {
                "first": f"0x{r.first:x}",
                "last": f"0x{r.last:x}",
                "instructions": r.instructions,
                "pct": r.pct,
                "starts": [f"0x{pc:x}" for pc in r.starts],
                "loops": [f"0x{pc:x}" for pc in r.loops],
            }
            for r in ranking.regions
        ],
    }


def cmd_rank(args: argparse.Namespace) -> int:
    from tools.emulator import coupled

    gen = pathlib.Path(args.gen) if args.gen else coupled.generated(args.device)
    regions = parse_region_entries((gen / "image.rs").read_text())
    interpreted, starts = read_capture(args.prefix)
    ranking = rank_regions(regions, interpreted, starts, args.min_pct)
    if args.json:
        print(json.dumps(ranking_json(ranking), indent=2))
    else:
        print(format_ranking(ranking))
    return 0


# Check


@dataclasses.dataclass(frozen=True)
class Budget:
    scenario: str
    scenarios_file: str
    seconds: float
    max_interpreted_per_audio_s: float
    max_block_entries_per_audio_s: float | None = None


def load_budgets(
    path: pathlib.Path = BUDGETS,
) -> dict[str, dict[str, Budget]]:
    """device -> fixture state ("first-boot", "reset") -> budget of that case."""
    raw = json.loads(path.read_text())
    return {
        device: {
            state: Budget(
                str(item["scenario"]),
                str(item["scenarios_file"]),
                float(item["seconds"]),
                float(item["max_interpreted_per_audio_s"]),
                (
                    float(item["max_block_entries_per_audio_s"])
                    if "max_block_entries_per_audio_s" in item
                    else None
                ),
            )
            for state, item in cases.items()
        }
        for device, cases in raw.items()
        if not device.startswith("_")
    }


def _dsp_rate(report: Mapping[str, object], scenario: str, name: str) -> float:
    """DSP instruction counter NAME / audio seconds of SCENARIO in a coupled-run report."""
    scenarios = report.get("scenarios")
    for sc in scenarios if isinstance(scenarios, list) else []:
        if sc.get("name") != scenario:
            continue
        split = sc.get("dsp_instructions")
        audio = float(sc.get("audio_seconds") or 0)
        if not isinstance(split, dict) or audio <= 0:
            raise SystemExit(
                f"fast_roots: scenario {scenario} has no DSP tier counts or audio time"
            )
        return float(split[name]) / audio
    raise SystemExit(f"fast_roots: scenario {scenario} is not in the report")


def interpreted_per_audio_second(report: Mapping[str, object], scenario: str) -> float:
    """interpreter instructions / audio seconds of SCENARIO in a coupled-run report."""
    return _dsp_rate(report, scenario, "interpreter")


def block_entries_per_audio_second(
    report: Mapping[str, object], scenario: str
) -> float:
    """generated-block entries / audio seconds of SCENARIO in a coupled-run report."""
    return _dsp_rate(report, scenario, "block_entries")


def over_budget(
    value: float, budget: Budget, block_entries: float | None = None
) -> list[str]:
    """The budgets that the measured rates exceed: "interpreted" and/or "block entries" (empty: within budget)."""
    over = []
    if value > budget.max_interpreted_per_audio_s:
        over.append("interpreted")
    limit = budget.max_block_entries_per_audio_s
    if limit is not None and block_entries is not None and block_entries > limit:
        over.append("block entries")
    return over


def sha256_file(path: pathlib.Path) -> str:
    with open(path, "rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def fixture_key(
    stock_sha256: str,
    files: Mapping[str, pathlib.Path],
    gen_files: Mapping[str, pathlib.Path],
    state: str = "first-boot",
) -> str:
    """sha256 over the fixture state, the stock firmware, the tracked input files and the generated core files (names and contents)."""
    record = {
        "version": 1,
        "state": state,
        "stock": stock_sha256,
        "files": sorted((name, sha256_file(p)) for name, p in files.items()),
        "gen": sorted((name, sha256_file(p)) for name, p in gen_files.items()),
    }
    text = json.dumps(record, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(text.encode()).hexdigest()


def tracked_files(
    specs: Iterable[str] | None = None, root: pathlib.Path = ROOT
) -> dict[str, pathlib.Path]:
    if specs is None:
        specs = key_specs()
    done = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", *specs],
        cwd=root, check=True, capture_output=True,
    )  # fmt: skip
    names = sorted(set(done.stdout.decode().split("\0")) - {""})
    return {n: root / n for n in names if (root / n).is_file()}


def current_key(device: str, state: str = "first-boot") -> str:
    from tools.devtools import emud_key
    from tools.emulator import coupled

    stock = ROOT / "firmware" / STOCK_SYX[device]
    if not stock.is_file():
        raise SystemExit(f"fast_roots: stock firmware {stock} is missing")
    gen = emud_key.tree_files(coupled.generated(device))
    return fixture_key(sha256_file(stock), tracked_files(), gen, state)


def fixture_store(device: str) -> pathlib.Path:
    from tools.devtools import verify_build

    return verify_build.store_root() / "fixtures" / device


def ensure_fixture(device: str, state: str = "first-boot") -> tuple[object, str]:
    """(Fixture, key) of the current runtime in STATE; built on a cache miss by tools/devtools/runtime_fixture.ts."""
    from tools.emulator import native_pgo

    key = current_key(device, state)
    store = fixture_store(device)
    final = store / key
    if final.is_dir():
        (final / "key.txt").touch()
    else:
        store.mkdir(parents=True, exist_ok=True)
        tmp = store / f".tmp-{os.getpid()}"
        shutil.rmtree(tmp, ignore_errors=True)
        print(
            f"fast_roots: no cached {device.upper()} {state} fixture for key {key[:12]}; building it",
            flush=True,
        )
        done = subprocess.run(
            [
                "node", "tools/devtools/runtime_fixture.ts",
                "--device", device, "--state", state, "--out", str(tmp),
            ],
            cwd=ROOT, check=False, env={**os.environ, **daemon_env()},
        )  # fmt: skip
        if done.returncode != 0:
            shutil.rmtree(tmp, ignore_errors=True)
            raise SystemExit(
                f"fast_roots: runtime_fixture failed for {device} (exit {done.returncode})"
            )
        (tmp / "key.txt").write_text(key + "\n")
        try:
            os.rename(tmp, final)
        except OSError:
            shutil.rmtree(tmp, ignore_errors=True)  # another run was first
            if not final.is_dir():
                raise
        from tools.devtools import verify_build

        verify_build.evict(store, key)
    return native_pgo.load_fixture(device, final), key


def measure(
    device: str, state: str, budget: Budget, seconds: float
) -> tuple[float, float, str]:
    """Replay the budget scenario on the current runtime: (interpreted instructions, block entries per audio second, fixture key)."""
    from tools.emulator import coupled, digi_env

    fixture, key = ensure_fixture(device, state)
    firmware = fixture.files["firmware"]  # type: ignore[attr-defined]
    dsp_image = fixture.files["dsp_image"]  # type: ignore[attr-defined]
    binary = coupled.ensure_binary(True, device, dsp_image, pgo=True)
    with tempfile.TemporaryDirectory(prefix="fast-roots-") as tmp:
        report = pathlib.Path(tmp) / "report.json"
        argv = [
            str(binary),
            *fixture.input_args(),  # type: ignore[attr-defined]
            "--scenarios", str(coupled.scenario_path(ROOT / budget.scenarios_file)),
            "--scenario", budget.scenario,
            "--seconds", str(seconds),
            "--out", str(report),
        ]  # fmt: skip
        env = {**os.environ}
        # The tier tables are keyed by the stock firmware (as the PGO replay runs do).
        env.update(digi_env.fast_env(env, syx=digi_env.base_firmware(firmware)))
        try:
            done = subprocess.run(
                argv, cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                text=True, check=False, timeout=RUN_TIMEOUT,
            )  # fmt: skip
        except subprocess.TimeoutExpired as exc:
            raise SystemExit(
                f"fast_roots: {device} run exceeded {RUN_TIMEOUT:.0f} s"
            ) from exc
        if done.returncode != 0 or not report.is_file():
            raise SystemExit(
                f"fast_roots: {device} run failed (exit {done.returncode}):\n{done.stderr[-1500:]}"
            )
        data = json.loads(report.read_text())
        return (
            interpreted_per_audio_second(data, budget.scenario),
            block_entries_per_audio_second(data, budget.scenario),
            key,
        )


HINT = (
    "The runtime patches probably bar generated regions. Capture with SHARC_PROFILE_PREFIX "
    "(docs/INSTRUMENTATION.md) and run `mise run perf:fast-roots -- rank ...`, then add "
    "`fallback_regions` in config/emulator.json."
)
HINT_BLOCK_ENTRIES = (
    "Too many block entries: the generated core lacks links for hot blocks. Profile with "
    "SHARC_PROFILE_PREFIX (docs/INSTRUMENTATION.md), merge the captures and regenerate the core."
)


def cmd_check(args: argparse.Namespace) -> int:
    budgets = load_budgets()
    failures: set[str] = set()
    for device in [d.strip() for d in args.device.split(",") if d.strip()]:
        if device not in budgets:
            raise SystemExit(f"fast_roots: no budget for {device} in {BUDGETS}")
        for state, budget in budgets[device].items():
            seconds = args.seconds if args.seconds is not None else budget.seconds
            value, entries, key = measure(device, state, budget, seconds)
            over = over_budget(value, budget, entries)
            limit = budget.max_block_entries_per_audio_s
            entry_text = (
                f", {entries:,.0f} block entries (budget {limit:,.0f})"
                if limit is not None
                else ""
            )
            print(
                f"{device} {state}: {value:,.0f} interpreted instructions per audio second "
                f"(budget {budget.max_interpreted_per_audio_s:,.0f}){entry_text}; "
                f"fixture {key[:12]} {'OVER BUDGET (' + ', '.join(over) + ')' if over else 'ok'}"
            )
            failures.update(over)
    if failures:
        if "interpreted" in failures:
            print(f"fast_roots: {HINT}", file=sys.stderr)
        if "block entries" in failures:
            print(f"fast_roots: {HINT_BLOCK_ENTRIES}", file=sys.stderr)
        return 1
    return 0


def cmd_fixtures(args: argparse.Namespace) -> int:
    budgets = load_budgets()
    for device in [d.strip() for d in args.device.split(",") if d.strip()]:
        if device not in budgets:
            raise SystemExit(f"fast_roots: no budget for {device} in {BUDGETS}")
        for state in budgets[device]:
            _fixture, key = ensure_fixture(device, state)
            print(f"{device} {state}: fixture {key[:12]}")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    rank = sub.add_parser("rank", help="rank barred regions from a capture")
    rank.add_argument(
        "--prefix", required=True, help="SHARC_PROFILE_PREFIX of the capture"
    )
    rank.add_argument("--device", required=True, choices=("dn2", "dt2"))
    rank.add_argument("--gen", help="generated core directory (default: the device's)")
    rank.add_argument("--min-pct", type=float, default=0.3)
    rank.add_argument("--json", action="store_true")
    check = sub.add_parser("check", help="check interpreted work against the budgets")
    check.add_argument("--device", required=True, help="dn2, dt2 or dn2,dt2")
    check.add_argument("--seconds", type=float, help="override the budget's run length")
    fixtures = sub.add_parser("fixtures", help="build or restore the fixtures only")
    fixtures.add_argument("--device", required=True, help="dn2, dt2 or dn2,dt2")
    args = ap.parse_args(argv)
    if args.cmd == "rank":
        return cmd_rank(args)
    return cmd_fixtures(args) if args.cmd == "fixtures" else cmd_check(args)


if __name__ == "__main__":
    raise SystemExit(main())
