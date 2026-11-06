"""Every machine module must pass this suite, whatever it renders (docs/TESTING.md "Machine conformance").

The cases come from the module's manifest (tools/patchkit/pk_conform.py): every parameter at its limits, parameters x notes, per-block
modulation, long notes and retriggers. Each case runs the module's DSP routine on the concrete SHARC+ core through core/dsp-voice
(tests/support/machine_rig.py) and checks, for every block, that the call returns, the context registers come back, the output is
finite and bounded, and the instruction count is within the budget. A module's own tests stay with the module.

One worker process per (module, device) runs all cases (tests/support/conform_worker.py, PyPy when uv can start it); the tests below
read its report, one test per case group. Select with DIGI_CONFORM_MODULES=<author/name>,... and DIGI_CONFORM_DEVICE=dt2|dn2
(`mise run module:check NAME` sets them). Reports go to out/runs/conformance/ (never committed).
"""

import json
import os
import shutil
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest
from support.paths import REPO_ROOT

from tools import python_env
from tools.patchkit import pk_conform, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

GROUPS = ("limits", "corners", "modulation", "long", "retrigger", "writes")
REPORTS = REPO_ROOT / "out" / "runs" / "conformance"
WORKER_TIMEOUT = 900
MAX_SHOWN = 6


def _wanted() -> list[pk_conform.MachineSpec]:
    mods = {m for m in os.environ.get("DIGI_CONFORM_MODULES", "").split(",") if m}
    dev = os.environ.get("DIGI_CONFORM_DEVICE", "")
    return [
        s
        for s in pk_conform.machine_specs()
        if (not mods or s.module in mods) and (not dev or s.device == dev)
    ]


SPECS = _wanted()


def _python() -> list[str]:
    """PyPy through uv when it can start (3 to 4 times faster on the concrete core), else this Python."""
    if shutil.which("uv"):
        probe = subprocess.run(
            [
                "uv",
                "run",
                "--no-project",
                "--python",
                "pypy3.11",
                "python",
                "-c",
                "pass",
            ],
            capture_output=True,
            cwd=REPO_ROOT,
        )
        if probe.returncode == 0:
            return ["uv", "run", "--no-project", "--python", "pypy3.11", "python"]
    return [sys.executable]


_lock = threading.Lock()
_results: dict[str, dict] = {}


def _run_worker(spec: pk_conform.MachineSpec, groups: tuple[str, ...] = ()) -> dict:
    env = python_env()
    env["PYTHONPATH"] = os.pathsep.join([env["PYTHONPATH"], str(REPO_ROOT / "tests")])
    cmd = [
        *_python(),
        "-m",
        "support.conform_worker",
        spec.module,
        spec.device,
        *[a for g in groups for a in ("--group", g)],
    ]
    r = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=REPO_ROOT,
        env=env,
        timeout=WORKER_TIMEOUT,
    )
    if r.returncode != 0 or not r.stdout.strip():
        return {
            "error": f"worker for {spec.label} failed ({r.returncode}):\n{r.stderr[-3000:]}{r.stdout[-500:]}"
        }
    report = json.loads(r.stdout.strip().splitlines()[-1])
    if groups:
        return report
    REPORTS.mkdir(parents=True, exist_ok=True)
    (REPORTS / f"{spec.module.replace('/', '__')}@{spec.device}.json").write_text(
        json.dumps(report, indent=1)
    )
    return report


@pytest.fixture
def report(request):
    """The worker report of this test's (module, device); the first test to ask starts the workers of every selected spec at once."""
    label = request.node.callspec.params["spec"]
    with _lock:
        if not _results:
            labels = {
                i.callspec.params["spec"]
                for i in request.session.items
                if hasattr(i, "callspec") and "spec" in i.callspec.params
            }
            todo = [s for s in SPECS if s.label in labels]
            first = {
                s.key: s for s in reversed(todo)
            }  # modules built from the same sources run once
            jobs = int(os.environ.get("DIGI_JOBS", max(1, (os.cpu_count() or 4) // 4)))
            with ThreadPoolExecutor(max_workers=max(1, min(jobs, len(first)))) as pool:
                done = dict(
                    zip(first, pool.map(_run_worker, first.values()), strict=True)
                )
            for s in todo:
                _results[s.label] = done[s.key]
    rep = _results[label]
    if "error" in rep:
        pytest.fail(rep["error"], pytrace=False)
    return rep


def _spec_ids() -> list:
    return [pytest.param(s.label, id=s.label) for s in SPECS]


@pytest.mark.parametrize("group", GROUPS)
@pytest.mark.parametrize("spec", _spec_ids())
def test_machine_conforms(spec, group, report):
    cases = [c for c in report["cases"] if c["group"] == group]
    assert cases, f"{spec}: no cases in group {group}"
    bad = [c for c in cases if c["failure"]]
    if bad:
        shown = "\n".join("  " + c["failure"] for c in bad[:MAX_SHOWN])
        more = (
            f"\n  ... and {len(bad) - MAX_SHOWN} more" if len(bad) > MAX_SHOWN else ""
        )
        pytest.fail(
            f"{spec} group {group}: {len(bad)} of {len(cases)} cases failed\n{shown}{more}",
            pytrace=False,
        )


@pytest.mark.parametrize("spec", _spec_ids())
def test_worst_case_instructions_are_reported(spec, report):
    """The measured worst case of the instruction budget, per voice-block and per note-on block (shown with -rA / -s)."""
    steady = max(c["steady"] for c in report["cases"])
    noteon = max(c["noteon"] for c in report["cases"])
    b = report["budget"]
    print(
        f"{spec}: worst block {steady} instructions (budget {b['voice_block']}), worst note-on block {noteon} (budget {b['note_on_block']})"
    )
    assert steady <= b["voice_block"] and noteon <= b["note_on_block"]


def test_the_suite_finds_machine_modules():
    assert (
        SPECS
        or os.environ.get("DIGI_CONFORM_MODULES")
        or os.environ.get("DIGI_CONFORM_DEVICE")
    ), "no machine module found"


NEGATIVES = {s.label: s for s in pk_conform.machine_specs(negative=True)}


@pytest.mark.parametrize("label", sorted(NEGATIVES))
def test_the_suite_rejects_a_machine_that_hangs(label):
    """The negative fixture (pk_conform.NEGATIVE) must fail: the validator is itself tested."""
    spec = NEGATIVES[label]
    if (
        os.environ.get("DIGI_CONFORM_MODULES")
        and spec.module not in os.environ["DIGI_CONFORM_MODULES"]
    ):
        pytest.skip("not selected")
    rep = _run_worker(spec, groups=("limits",))
    assert "error" not in rep, rep.get("error")
    failures = [c["failure"] for c in rep["cases"] if c["failure"]]
    assert failures and all(pk_conform.NEGATIVE[spec.module] in f for f in failures), (
        failures[:2]
    )
