#!/usr/bin/env python3
"""Shared library of the patchkit emulator proofs (tools/patchkit/pk_proof_*.py).

A proof tool declares *parts* (independent units: each sets up its own references) and a `Proof` subclass; this module
gives it

  * one coupled cold boot per (image, ready inputs), cached: the daemon keeps the ready pair of a patched MAIN in
    `<run dir>/patched`; every daemon of a proof run shares one such directory (`OUT/.cache`, see
    `pk_harness.link_shared`) and one card path, and a lock makes the first process boot cold while the others wait
    and restore the pair (0.3 s instead of 13 s);
  * sessions pooled inside a process (`Proof.session`): the same image is reused by the next part and only `restart`ed;
  * parts in parallel processes (`--jobs N`, default: the free cores / 2, at most 6; each process has its own daemons);
  * a `--quick` tier (the parts and the branches of a part marked quick: selection and type, pitch, one parameter
    effect, persistence, no fault/halt/stall), the full tier unchanged;
  * per-part summary lines with the wall time split (open, restart, run, rest = analysis and panel waits) and a final
    PASS/FAIL, and `out/summary.json`;
  * `verify-restore`: the boot cache gives the same state digest and PCM as a cold boot.

    uv run python -m tools.patchkit.pk_proofkit verify-restore --device dt2 --fw 1.17 --mods core/machines,core/params

Tool layout:

    class MyProof(proofkit.Proof): ...                  # check(), session(), fresh() etc.
    PARTS = [proofkit.Part("a", "select", MyProof.part_a, quick=True, weight=120), ...]
    def main(): return proofkit.run_tool(__file__, MyProof, PARTS, ap, args)

Exit 0 only when every check of every selected part passes.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import re
import resource
import subprocess
import sys
import threading
import time
import traceback
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any

import numpy as np

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

from tools import python_env  # noqa: E402
from tools.patchkit import pk_harness as H  # noqa: E402

MAX_POOL = 3  # live daemons per process


@dataclasses.dataclass
class Part:
    """One independent unit of a proof. `fn(proof)` runs it. `quick` parts run in the --quick tier (the part itself
    may read `proof.quick`); `full=False` marks a part that only the quick tier runs (a shortened variant of parts that
    the full tier runs whole). `group` lets `--only b` name several parts (b1, b2, ...). `weight` is a rough wall time
    in seconds: the heaviest parts start first."""

    name: str
    title: str
    fn: Callable[[Any], None]
    quick: bool = True
    weight: int = 60
    full: bool = True
    group: str = ""


@dataclasses.dataclass
class CheckResult:
    name: str
    ok: bool
    detail: str = ""


def analyse(pcm: np.ndarray) -> dict:
    """RMS and peak of the active part, crest factor, spectral shares, largest sample-to-sample step."""
    ch = [pcm[0::2].astype(np.float64), pcm[1::2].astype(np.float64)]
    act = np.flatnonzero(np.abs(ch[0]) > 1e-6)
    if len(act) < 2000:
        return {"active_samples": int(len(act)), "silent": True}
    lo, hi = act[0], act[-1]
    seg = ch[0][lo:hi]
    rms = float(np.sqrt(np.mean(seg**2)))
    peak = float(np.max(np.abs(seg)))
    n = 1 << int(np.floor(np.log2(len(seg))))
    spec = np.abs(np.fft.rfft(seg[len(seg) // 2 - n // 2 :][:n] * np.hanning(n))) ** 2
    k = int(np.argmax(spec))
    tot = float(spec.sum())
    core = float(spec[max(0, k - 3) : k + 4].sum())
    freqs = np.fft.rfftfreq(n, 1 / 48000)
    return {
        "active_samples": int(len(act)),
        "silent": False,
        "rms": rms,
        "peak": peak,
        "crest_db": 20 * np.log10(peak / rms),
        "peak_hz": float(freqs[k]),
        "non_fundamental": 1
        - core / tot,  # power outside the strongest partial (a sine: harmonic content)
        "hf_share": float(spec[freqs > 2000].sum() / tot),
        "max_step": max(float(np.max(np.abs(np.diff(c[lo:hi])))) for c in ch),
    }


def aligned_equal(
    ref: np.ndarray, test: np.ndarray, max_shift: int = 512
) -> tuple[bool, int | None]:
    """Is TEST equal to REF over REF's active part, possibly shifted by whole stereo frames? The cold-booted
    patched ready state can sit one DSP block (64 samples) away from the stock one: a capture phase, not audio."""
    for k in sorted(range(-max_shift, max_shift + 1, 2), key=abs):
        x, y = (ref[k:], test) if k >= 0 else (ref, test[-k:])
        n = min(len(x), len(y))
        x, y = x[:n], y[:n]
        act = np.flatnonzero(np.abs(x) > 1e-6)
        if len(act) > 1000 and np.array_equal(x[act[0] : act[-1]], y[act[0] : act[-1]]):
            return True, k
    return False, None


def slug(text: str) -> str:
    return re.sub(r"[^A-Za-z0-9.+-]+", "_", text).strip("_")


# ----- sessions -------------------------------------------------------------------------------------------------


class Lease:
    """`with proof.session(...) as s:` — the session goes back to the pool on a clean exit and is dropped when the
    block raised (a faulted daemon must not serve the next part)."""

    def __init__(self, proof: Proof, key: tuple, session: H.Session, pooled: bool):
        self.proof, self.key, self.session, self.pooled = proof, key, session, pooled
        proof.busy[key] = proof.busy.get(key, 0) + 1

    def __enter__(self) -> H.Session:
        return self.session

    def __exit__(self, exc_type: object, *exc: object) -> None:
        with self.proof.lock:
            self.proof.busy[self.key] -= 1
            if self.proof.busy[self.key] <= 0:
                del self.proof.busy[self.key]
                if exc_type is not None or not self.pooled:
                    self.proof.drop(self.key)


class Proof:
    """Base of the proof classes: checks, pooled sessions, timings."""

    tool = "proof"
    device = "dt2"
    fw = "1.17"
    ready_env: dict[
        str, str
    ] = {}  # e.g. {"DIGI_PK_READY_DN2_112": ".../ready-fastweb-dn2"}

    def __init__(
        self,
        out: Path,
        quick: bool = False,
        part: str = "",
        cache_dir: Path | None = None,
    ):
        self.base_out = out
        self.out = out
        self.quick = quick
        self.part = part
        self.cache_dir = cache_dir if cache_dir is not None else H.SHARED_CACHE
        out.mkdir(parents=True, exist_ok=True)
        self.results: list[CheckResult] = []
        self.failed: list[str] = []
        self.pool: dict[tuple, H.Session] = {}
        self.busy: dict[
            tuple, int
        ] = {}  # sessions inside a `with` block (never evicted)
        self.lock = threading.RLock()  # the pool and the check list, for `parallel`
        self.timings: dict[str, float] = {}
        for k, v in self.ready_env.items():
            if k not in os.environ and Path(v).is_dir():
                os.environ[k] = v

    # -- checks
    def check(self, name: str, ok: bool, detail: object = "") -> bool:
        text = str(detail)
        with self.lock:
            print(
                f"  {'ok  ' if ok else 'FAIL'} {name}{': ' + text if text else ''}",
                flush=True,
            )
            self.results.append(CheckResult(name, bool(ok), text))
            if not ok:
                self.failed.append(name)
        return bool(ok)

    def parallel(self, *calls: Callable[[], Any]) -> list[Any]:
        """Run CALLS in threads (each on its own daemon: give each a distinct session tag) and return their results
        in order; the first exception is raised after all have finished."""
        out: list[Any] = [None] * len(calls)
        errors: list[BaseException] = []
        width = threading.Semaphore(H.part_jobs(len(calls)))  # DIGIKIT_PART_JOBS

        def run(i: int) -> None:
            with width:
                try:
                    out[i] = calls[i]()
                except BaseException as e:  # noqa: BLE001
                    errors.append(e)

        threads = [threading.Thread(target=run, args=(i,)) for i in range(len(calls))]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        if errors:
            raise errors[0]
        return out

    # -- sessions
    def session(
        self,
        mods: Sequence[str],
        tag: str = "",
        *,
        device: str | None = None,
        fw: str | None = None,
        env: dict[str, str] | None = None,
        pool: bool = True,
        image: Path | None = None,
    ) -> Lease:
        """A coupled daemon on the patch of MODS (stock when empty) for the part. Pooled sessions are reused by the
        next call with the same arguments; callers `restart` them (`fresh`) themselves."""
        with self.lock:
            return self._session(mods, tag, device, fw, env, pool, image)

    def _session(
        self,
        mods: Sequence[str],
        tag: str,
        device: str | None,
        fw: str | None,
        env: dict[str, str] | None,
        pool: bool,
        image: Path | None,
    ) -> Lease:
        device, fw = device or self.device, fw or self.fw
        key = (
            device,
            fw,
            tuple(sorted(mods)),
            tuple(sorted((env or {}).items())),
            str(image),
            tag,
        )
        if pool and key in self.pool:
            return Lease(self, key, self.pool[key], True)
        self.drop(key)
        while len(self.pool) >= MAX_POOL:
            idle = [k for k in self.pool if k not in self.busy]
            if not idle:
                break
            self.drop(idle[0])
        cell = H.Cell(device, fw, list(mods))
        name = slug(f"{self.tool}-{self.part}-{tag or cell.modset}-{device}{fw}")
        s = H.Session(
            cell,
            H.Goldens(),
            log=None,
            run_name=name,
            env=env,
            cache_dir=self.cache_dir,
        )
        try:
            s.open(image=image)
        except BaseException:
            self.absorb(s)
            s.close()
            raise
        self.pool[key] = s
        return Lease(self, key, s, pool)

    def drop(self, key: tuple) -> None:
        s = self.pool.pop(key, None)
        if s is not None:
            self.absorb(s)
            s.close()

    def absorb(self, s: H.Session) -> None:
        for k, v in s.timings.items():
            self.timings[k] = self.timings.get(k, 0.0) + v

    def total_timings(self) -> dict[str, float]:
        """Wall seconds by kind over the closed and the live sessions (see `Session.timings`)."""
        total = dict(self.timings)
        for s in self.pool.values():
            for k, v in s.timings.items():
                total[k] = total.get(k, 0.0) + v
        return total

    def close(self) -> None:
        for key in list(self.pool):
            self.drop(key)

    def snap(self, s: H.Session, name: str) -> str:
        """Save the OLED frame as PNG; the hash of its bytes."""
        from tools.control.digi_client import oled_png

        _, raw = s.emu.frame()
        (self.out / f"{name}.png").write_bytes(oled_png(raw))
        return hashlib.sha256(raw).hexdigest()[:12]


# ----- running parts --------------------------------------------------------------------------------------------


def run_part(
    proof_cls: type[Proof],
    part: Part,
    out: Path,
    quick: bool,
    cache_dir: Path | None,
    proof: Proof | None = None,
) -> dict[str, Any]:
    """One part in this process; the result record. A crash of the part is a failed check, not the end of the run."""
    own = proof is None
    proof = proof or proof_cls(out, quick=quick, part=part.name, cache_dir=cache_dir)
    proof.part = part.name
    proof.out = proof.base_out / part.name  # parallel parts never write the same file
    proof.out.mkdir(parents=True, exist_ok=True)
    n0 = len(proof.results)
    t0 = time.monotonic()
    before = proof.total_timings()
    cpu0 = _children_cpu()
    print(f"{part.name}  {part.title}" + (" (quick)" if quick else ""), flush=True)
    try:
        part.fn(proof)
    except BaseException as e:  # noqa: BLE001  a Failure/EmuError/assert of a part is its verdict
        if isinstance(e, KeyboardInterrupt):
            raise
        tb = traceback.format_exc().strip().splitlines()
        proof.check(
            f"part {part.name} crashed",
            False,
            f"{type(e).__name__}: {e} [{tb[-3].strip()}]",
        )
        traceback.print_exc()
        for key in list(proof.pool):  # the daemons may be in any state
            proof.drop(key)
    wall = time.monotonic() - t0
    after = proof.total_timings()
    delta = {k: after.get(k, 0.0) - before.get(k, 0.0) for k in after}
    if own:
        proof.close()
    results = proof.results[n0:]
    cpu = _children_cpu() - cpu0
    return {
        "part": part.name,
        "title": part.title,
        "ok": all(r.ok for r in results) and bool(results),
        "checks": [dataclasses.asdict(r) for r in results],
        "seconds": round(wall, 1),
        "open_s": round(delta.get("spawn_s", 0) + delta.get("load_s", 0), 1),
        "reset_s": round(delta.get("reset_s", 0), 1),
        "run_s": round(delta.get("run_s", 0), 1),
        "emu_s": round(delta.get("emu_ms", 0) / 1000, 1),
        "cpu_s": round(cpu, 1),
    }


def _children_cpu() -> float:
    """CPU seconds (user + system) of the reaped child processes: the daemons of closed sessions."""
    r = resource.getrusage(resource.RUSAGE_CHILDREN)
    return r.ru_utime + r.ru_stime


def summary_line(r: dict[str, Any]) -> str:
    n = len(r["checks"])
    bad = [c for c in r["checks"] if not c["ok"]]
    rest = max(r["seconds"] - r["open_s"] - r["reset_s"] - r["run_s"], 0)
    state = "PASS" if r["ok"] else "FAIL"
    return (
        f"part {r['part']:<3} {state}  {n:>3} checks  {r['seconds']:>6.1f} s  "
        f"(open {r['open_s']:.0f}, restart {r['reset_s']:.0f}, run {r['run_s']:.0f} for {r['emu_s']:.0f} emulated, "
        f"other {rest:.0f}; daemon cpu {r.get('cpu_s', 0):.0f})"
        + (f"  FAILED: {', '.join(c['name'] for c in bad)}" if bad else "")
    )


default_jobs = H.default_jobs


def add_args(ap: argparse.ArgumentParser, parts: Sequence[Part]) -> None:
    """The options every proof tool has (`--out` is None unless given: the tool picks its directory)."""
    ap.add_argument("--out", type=Path)
    ap.add_argument(
        "--only",
        help=f"comma-separated parts ({','.join(p.name for p in parts)}); default: all (quick: the quick ones)",
    )
    ap.add_argument(
        "--quick",
        action="store_true",
        help="the short tier: the quick parts with shorter renders and fewer settings; the checks that catch breakage",
    )
    ap.add_argument(
        "--jobs",
        type=int,
        default=0,
        help="parts in parallel processes (default: free cores / 2, at most 6; 1 runs them here, in order)",
    )
    ap.add_argument(
        "--result", type=Path, help=argparse.SUPPRESS
    )  # child: write the part's record here


def _strip_options(argv: list[str], names: Sequence[str]) -> list[str]:
    out: list[str] = []
    skip = False
    for a in argv:
        if skip:
            skip = False
            continue
        if a in names:
            skip = True
            continue
        if any(a.startswith(n + "=") for n in names):
            continue
        out.append(a)
    return out


def run_tool(
    script: str,
    proof_cls: type[Proof],
    parts: Sequence[Part],
    args: argparse.Namespace,
    argv: list[str] | None = None,
    label: str = "",
) -> int:
    """Run the selected parts (in order here, or in parallel processes) and print the verdict."""
    argv = list(sys.argv[1:] if argv is None else argv)
    out: Path = args.out
    out.mkdir(parents=True, exist_ok=True)
    by_name = {p.name: p for p in parts}
    if args.only:
        names = []
        for n in args.only.split(","):
            hit = [p.name for p in parts if n in (p.name, p.group)]
            if not hit:
                print(f"unknown part {n!r}; the parts are {list(by_name)}")
                return 2
            names += [h for h in hit if h not in names]
    elif args.quick:
        names = [p.name for p in parts if p.quick]
    else:
        names = [p.name for p in parts if p.full]
    cache_dir = H.SHARED_CACHE
    label = label or Path(script).stem
    started = time.monotonic()
    records: list[dict[str, Any]] = []

    jobs = args.jobs or default_jobs(len(names))
    if args.result is not None or jobs == 1 or len(names) == 1:
        # in this process, in the order given; sessions are pooled across the parts
        proof = (
            None
            if len(names) == 1
            else proof_cls(out, quick=args.quick, cache_dir=cache_dir)
        )
        try:
            for n in names:
                rec = run_part(proof_cls, by_name[n], out, args.quick, cache_dir, proof)
                records.append(rec)
                print(summary_line(rec), flush=True)
        finally:
            if proof is not None:
                cpu0 = _children_cpu()
                proof.close()
                records[-1]["cpu_s"] = round(
                    records[-1].get("cpu_s", 0) + _children_cpu() - cpu0, 1
                )
        if args.result is not None:
            args.result.write_text(json.dumps(records[0]))
            return 0
    else:
        records = run_parallel(script, names, by_name, argv, out, jobs, label)
    failed = [r["part"] for r in records if not r["ok"]]
    wall = time.monotonic() - started
    total = sum(r["seconds"] for r in records)
    (out / "summary.json").write_text(
        json.dumps(
            {
                "tool": label,
                "quick": args.quick,
                "wall_s": round(wall, 1),
                "parts": records,
            },
            indent=1,
        )
    )
    verdict = "PASS" if not failed else f"FAIL: parts {', '.join(failed)}"
    print(
        f"{verdict}  {label}{' (quick)' if args.quick else ''}: {len(records)} parts, wall {wall:.0f} s "
        f"(sum of parts {total:.0f} s)",
        flush=True,
    )
    return 1 if failed else 0


def run_parallel(
    script: str,
    names: list[str],
    by_name: dict[str, Part],
    argv: list[str],
    out: Path,
    jobs: int,
    label: str,
) -> list[dict[str, Any]]:
    base = _strip_options(argv, ("--only", "--jobs", "--result"))
    logs = out / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    queue = sorted(names, key=lambda n: -by_name[n].weight)
    running: dict[str, tuple[subprocess.Popen[bytes], Path, Path, float]] = {}
    records: dict[str, dict[str, Any]] = {}
    print(
        f"{label}: {len(names)} parts, {jobs} in parallel; logs in {logs}", flush=True
    )
    while queue or running:
        while queue and len(running) < jobs:
            n = queue.pop(0)
            log, res = logs / f"{n}.log", logs / f"{n}.json"
            res.unlink(missing_ok=True)
            with log.open("wb") as f:
                p = subprocess.Popen(
                    [
                        sys.executable,
                        "-u",
                        "-m",
                        f"tools.patchkit.{Path(script).stem}",
                        *base,
                        "--only",
                        n,
                        "--jobs",
                        "1",
                        "--result",
                        str(res),
                    ],
                    stdout=f,
                    stderr=subprocess.STDOUT,
                    cwd=ROOT,
                    env=python_env(),
                )
            running[n] = (p, log, res, time.monotonic())
        time.sleep(0.5)
        for n, (p, log, res, t0) in list(running.items()):
            rc = p.poll()
            if rc is None:
                continue
            del running[n]
            text = log.read_text(errors="replace")
            try:
                rec = json.loads(res.read_text())
            except (OSError, ValueError):
                rec = {
                    "part": n,
                    "title": by_name[n].title,
                    "ok": False,
                    "checks": [
                        {
                            "name": f"part {n} produced no result",
                            "ok": False,
                            "detail": f"exit {rc}; last log lines: {' | '.join(text.strip().splitlines()[-3:])}",
                        }
                    ],
                    "seconds": round(time.monotonic() - t0, 1),
                    "open_s": 0,
                    "reset_s": 0,
                    "run_s": 0,
                    "emu_s": 0,
                }
            records[n] = rec
            print(summary_line(rec), flush=True)
            for c in rec["checks"]:
                if not c["ok"]:
                    print(f"     FAIL {c['name']}: {c['detail']}", flush=True)
    return [records[n] for n in names if n in records]


# ----- verify-restore -------------------------------------------------------------------------------------------


def _probe(cell: H.Cell, cache: Path, env: dict[str, str], tag: str) -> dict[str, Any]:
    """Open the cell's image on CACHE and run one fixed script; the digests."""
    import numpy as np  # noqa: F401

    from tools.patchkit import hook_ab

    s = H.Session(
        cell,
        H.Goldens(),
        log=None,
        run_name=f"verify-restore-{tag}",
        env=env,
        cache_dir=cache,
    )
    t0 = time.monotonic()
    with s:
        s.open()
        opened = time.monotonic() - t0
        emu = s.emu
        assert emu is not None
        loaded = emu.state_digest()
        s.reset()
        s.run_ms(1500)
        wav = cache / f"probe-{tag}.wav"
        emu.call("pcm.tap", action="capture", path=str(wav))
        s.do_hold("TRIG 1", 400)
        s.run_ms(600)
        emu.call("pcm.tap", action="capture_stop")
        s.health("probe")
        pcm = hook_ab.read_wav(wav)
        return {
            "start_used": s.load_report.get("start_used"),
            "cache_hit": s.load_report.get("cache_hit"),
            "open_s": round(opened, 1),
            "digest_loaded": loaded,
            "digest_end": emu.state_digest(),
            "pcm_sha": hashlib.sha256(pcm.tobytes()).hexdigest()[:16],
            "pcm_samples": int(len(pcm)),
        }


def verify_restore(a: argparse.Namespace) -> int:
    mods = [m for m in a.mods.split(",") if m]
    env = dict(kv.split("=", 1) for kv in a.env)
    cell = H.Cell(a.device, a.fw, mods)
    cache = ROOT / "out" / "runs" / "patchkit" / f".verify-restore-{slug(cell.name)}"
    if cache.exists():
        subprocess.run(["rm", "-rf", str(cache)], check=True)
    cold = _probe(cell, cache, env, "cold")
    warm = _probe(cell, cache, env, "warm")
    print("cold boot  ", json.dumps(cold))
    print("cache hit  ", json.dumps(warm))
    same = {
        k: cold[k] == warm[k]
        for k in ("digest_loaded", "digest_end", "pcm_sha", "pcm_samples")
    }
    ok = all(same.values()) and (not mods or warm["start_used"] == "snapshot-cache")
    print(("PASS" if ok else "FAIL") + f" verify-restore {cell.name}: identical {same}")
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    v = sub.add_parser(
        "verify-restore",
        help="cold boot vs boot-cache restore: same state digest and PCM",
    )
    v.add_argument("--device", default="dt2")
    v.add_argument("--fw", default="1.17")
    v.add_argument("--mods", default="", help="comma-separated modules (empty: stock)")
    v.add_argument(
        "--env", action="append", default=[], help="KEY=VALUE for the daemon"
    )
    a = ap.parse_args(argv)
    os.chdir(ROOT)
    return verify_restore(a)


if __name__ == "__main__":
    sys.exit(main())
