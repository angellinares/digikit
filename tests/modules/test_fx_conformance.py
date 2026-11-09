"""Every effect module must pass this suite, whatever it does (docs/TESTING.md "FX conformance").

The cases come from tools/patchkit/pk_conform.py `fx_cases`: test signals (silence, DC, square, noise, impulse, sine, ramp), the parameter words at
their limits, per-block modulation, an effect that is switched on and off, and a long run. Each runs the effect behind core/fx on the concrete
SHARC+ core (tests/support/fx_rig.py) and checks, for every block, that the hook returns, the context and the stock function's registers come
back, the adapter did not have to refuse the block, the module wrote only memory it owns, and the instruction count is within the budget.

One worker per module (tests/support/fx_conform_worker.py, PyPy when uv can start it); the tests read its report, one test per case group.
Select with DIGI_CONFORM_MODULES=test/fx-gain,... (`mise run module:check NAME` sets it). Reports go to out/runs/conformance/ (never committed).
"""

import json
import os
import shutil
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest
from support.fx_rig import FxRig, Row
from support.paths import REPO_ROOT

from tools import python_env
from tools.patchkit import pk_conform, pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

GROUPS = ("signals", "params", "modulation", "switching", "long")
REPORTS = REPO_ROOT / "out" / "runs" / "conformance"
WORKER_TIMEOUT = 900
MAX_SHOWN = 6


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


def _wanted() -> list[pk_conform.FxSpec]:
    mods = {m for m in os.environ.get("DIGI_CONFORM_MODULES", "").split(",") if m}
    return [s for s in pk_conform.fx_specs() if not mods or s.module in mods]


SPECS = _wanted()
_lock = threading.Lock()
_results: dict[str, dict] = {}


def _run_worker(spec: pk_conform.FxSpec, groups: tuple[str, ...] = ()) -> dict:
    env = python_env()
    env["PYTHONPATH"] = os.pathsep.join([env["PYTHONPATH"], str(REPO_ROOT / "tests")])
    cmd = [
        *_python(),
        "-m",
        "support.fx_conform_worker",
        spec.module,
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
    (REPORTS / f"{spec.module.replace('/', '__')}@fx.json").write_text(
        json.dumps(report, indent=1)
    )
    return report


@pytest.fixture
def report(request):
    """The worker report of this test's module; the first test to ask starts the workers of every selected module at once."""
    label = request.node.callspec.params["spec"]
    with _lock:
        if not _results:
            labels = {
                i.callspec.params["spec"]
                for i in request.session.items
                if hasattr(i, "callspec") and "spec" in i.callspec.params
            }
            todo = [s for s in SPECS if s.label in labels]
            jobs = int(os.environ.get("DIGI_JOBS", max(1, (os.cpu_count() or 4) // 4)))
            with ThreadPoolExecutor(
                max_workers=max(1, min(jobs, len(todo) or 1))
            ) as pool:
                done = list(pool.map(_run_worker, todo))
            for s, rep in zip(todo, done, strict=True):
                _results[s.label] = rep
    rep = _results[label]
    if "error" in rep:
        pytest.fail(rep["error"], pytrace=False)
    return rep


def _spec_ids() -> list:
    return [pytest.param(s.label, id=s.label) for s in SPECS]


@pytest.mark.parametrize("group", GROUPS)
@pytest.mark.parametrize("spec", _spec_ids())
def test_effect_conforms(spec, group, report):
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
    """The measured worst case of a call (the effect plus core/fx), against the budget (shown with -rA / -s)."""
    steady = max(c["steady"] for c in report["cases"])
    b = report["budget"]
    print(
        f"{spec}: worst block {steady} instructions (budget {b['effect']} for the effect + {b['adapter']} for core/fx)"
    )
    assert steady <= b["effect"] + b["adapter"]


def test_the_suite_finds_effect_modules():
    assert SPECS or os.environ.get("DIGI_CONFORM_MODULES"), "no effect module found"


NEGATIVES = {s.label: s for s in pk_conform.fx_specs(negative=True)}


@pytest.mark.parametrize("label", sorted(NEGATIVES))
def test_the_suite_rejects_an_effect_that_writes_a_nan(label):
    """The negative fixture (pk_conform.FX_NEGATIVE) must fail: the validator is itself tested."""
    spec = NEGATIVES[label]
    if (
        os.environ.get("DIGI_CONFORM_MODULES")
        and spec.module not in os.environ["DIGI_CONFORM_MODULES"]
    ):
        pytest.skip("not selected")
    rep = _run_worker(spec, groups=("signals",))
    assert "error" not in rep, rep.get("error")
    failures = [c["failure"] for c in rep["cases"] if c["failure"]]
    assert failures and all(
        pk_conform.FX_NEGATIVE[spec.module] in f for f in failures
    ), failures[:2]


def test_the_write_set_check_finds_a_store_into_the_input_block():
    """An effect that stores into the master block (read only for it) is caught by `foreign_stores`; a well-behaved one is not."""
    rig = FxRig([Row("_te_stray", "_te_state_a")], record_stores=True)
    rig.block([0.25] * 64)
    found = rig.foreign_stores(16)
    assert found and "master block" in found[0], found
    ok = FxRig([Row("_te_half", "_te_state_a")], record_stores=True)
    ok.block([0.25] * 64)
    assert ok.foreign_stores(16) == []
