"""The CPU budget of the verify gate (tools/devtools/verify_budget.py) and what hangs on it."""

from __future__ import annotations

import random
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

from tools.devtools import fx_tier, verify, verify_budget

BUDGETS = (1, 2, 3, 4, 8, 14, 16, 32, 64)
ALL_NODES = [*verify.STEPS, "build-dt2", "build-daemons"]
GB = verify_budget.GB
# What a gate step passes down (verify runs this file inside the gate, which sets these): the tests below
# must not depend on the caller's environment.
CALLER_VARS = (
    *verify_budget.step_env("", 1),
    "DIGI_VERIFY_BUDGET",
    "DIGI_EMUD",
    "DIGI_EMUD_DT2",
    "DIGI_EMUD_DN2",
)


@pytest.fixture(autouse=True)
def clean_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    for key in CALLER_VARS:
        monkeypatch.delenv(key, raising=False)


@pytest.mark.parametrize("total", BUDGETS)
def test_shares_never_exceed_the_budget(total: int) -> None:
    """Random schedules: nodes arrive in plan order, finish in random order; the shares of the running
    nodes never add up to more than the budget, every node gets at least one core, and all finish."""
    rng = random.Random(total)
    for _ in range(50):
        nodes = list(ALL_NODES)
        rng.shuffle(nodes)
        budget = verify_budget.Budget(total)
        pending = list(nodes)
        finished = 0
        while pending or budget.running:
            for node in list(pending):
                share = budget.admit(node)
                if share is not None:
                    pending.remove(node)
                    assert 1 <= share <= verify_budget.want_cores(node, total)
                assert sum(budget.running.values()) <= total
                assert len(budget.running) <= verify_budget.MAX_STEPS
            assert budget.running, "nothing running and nothing admitted: deadlock"
            done = rng.choice(sorted(budget.running))
            budget.release(done)
            finished += 1
        assert finished == len(nodes)


@pytest.mark.parametrize("total", BUDGETS)
def test_a_node_alone_always_starts(total: int) -> None:
    for node in ALL_NODES:
        assert verify_budget.Budget(total).admit(node) is not None


def test_a_node_waits_for_half_of_its_ask() -> None:
    budget = verify_budget.Budget(14)
    assert budget.admit("tier-sweep") == verify_budget.want_cores("tier-sweep", 14)
    free = budget.free
    want = verify_budget.want_cores("pins", 14)
    assert (budget.admit("pins") is None) == (free < -(-want // 2))


@pytest.mark.parametrize("total", BUDGETS)
def test_wave_plan_fits_and_covers_every_node(total: int) -> None:
    for steps in (verify.EVERYTHING, verify.STEPS, ("rust-tests",), ("pins",)):
        nodes, deps = verify.plan(steps, ["tests/digikit"])
        waves = verify_budget.wave_plan(nodes, deps, total)
        assert sorted(n for w in waves for n, _ in w) == sorted(nodes)
        for wave in waves:
            assert sum(share for _, share in wave) <= total
        earlier: set[str] = set()
        for wave in waves:  # a node runs after its dependencies
            for node, _ in wave:
                assert set(deps[node]) <= earlier
            earlier |= {n for n, _ in wave}


@pytest.mark.parametrize("total", BUDGETS)
@pytest.mark.parametrize("node", ALL_NODES)
def test_inner_pools_fit_the_share(node: str, total: int) -> None:
    w = verify_budget.workers_for(node, total)
    assert min(w.cargo, w.libtest, w.rayon, w.emulators, w.py_files, w.per_file) >= 1
    assert w.cargo <= total and w.rayon <= total and w.node <= total
    if total >= verify_budget.EMULATOR_CORES:
        assert w.emulators * verify_budget.EMULATOR_CORES <= total
    if node == "python-tests":
        # files at once x what one file holds (its cargo, or its daemons of two threads each)
        assert w.py_files * w.cargo <= max(total, w.py_files)
        assert w.py_files * w.per_file * verify_budget.EMULATOR_CORES <= max(
            total, w.py_files * 2
        )


# ---- builds: once, in one place --------------------------------------------------------------------------------


def test_no_build_node_without_a_step_that_needs_it() -> None:
    nodes, deps = verify.plan(("rust-tests", "web"), None)
    assert sorted(nodes) == ["host", "rust-tests", "web"]
    assert deps["rust-tests"] == ()
    nodes, _ = verify.plan(("python-tests",), ["tests/devtools/test_layercheck.py"])
    assert "build-daemons" not in nodes
    for tests in (
        ["tests/digikit"],
        ["tests/patchkit"],
        ["tests/digikit/test_menu_dt2.py"],
    ):
        nodes, deps = verify.plan(("python-tests",), tests)
        assert deps["python-tests"] == ("build-daemons",)
    nodes, _ = verify.plan(("dt2-check",), None)
    assert nodes.index("build-dt2") < nodes.index("dt2-check") and "build" not in nodes


# ---- AOT versus interpreter equality on a fixed subset ----------------------------------------------------------


@pytest.mark.parametrize("path", sorted(fx_tier.NO_AOT_SUBSET))
def test_no_aot_subset_names_exist(path: str) -> None:
    from tools.emulator.coupled import scenario_document

    doc = scenario_document(REPO_ROOT / path)
    names = {s["name"] for s in doc["scenarios"]}
    assert set(fx_tier.NO_AOT_SUBSET[path]) <= names
    assert len(fx_tier.NO_AOT_SUBSET[path]) == len(set(fx_tier.NO_AOT_SUBSET[path]))
    seconds = {s["name"]: float(s.get("seconds") or 0) for s in doc["scenarios"]}
    assert all(seconds[n] > 0 for n in fx_tier.NO_AOT_SUBSET[path])


def test_tier_names_subset_and_full() -> None:
    path = "tests/regression/scenarios/dn2-sendfx.ts"
    seconds = {f"s{i:03d}": 1.0 for i in range(20)}
    with pytest.raises(SystemExit):  # a subset name that the file lacks
        fx_tier.tier_names(path, seconds, 7, False)
    assert fx_tier.tier_names(path, seconds, 7, True) == sorted(seconds)[::7]
    assert fx_tier.tier_names(path, seconds, 1, True) == sorted(seconds)
    real = {n: 1.0 for n in fx_tier.NO_AOT_SUBSET[path]}
    assert fx_tier.tier_names(path, real, 1, False) == list(fx_tier.NO_AOT_SUBSET[path])


def test_part_jobs_bounds_the_parallel_parts(monkeypatch: pytest.MonkeyPatch) -> None:
    import threading
    import time

    from tools.patchkit import pk_digikit_scen, pk_harness, pk_proofkit

    assert pk_harness.part_jobs(4) == 4  # unset: all parts, as before
    monkeypatch.setenv("DIGIKIT_PART_JOBS", "1")
    assert pk_harness.part_jobs(4) == 1
    monkeypatch.setenv("DIGIKIT_PART_JOBS", "9")
    assert pk_harness.part_jobs(3) == 3

    def peak(run: object, width: str | None) -> int:
        if width is None:
            monkeypatch.delenv("DIGIKIT_PART_JOBS", raising=False)
        else:
            monkeypatch.setenv("DIGIKIT_PART_JOBS", width)
        live = top = 0
        lock = threading.Lock()

        def part() -> None:
            nonlocal live, top
            with lock:
                live += 1
                top = max(top, live)
            time.sleep(0.05)
            with lock:
                live -= 1

        run([part] * 4)  # type: ignore[operator]
        return top

    ctx = pk_digikit_scen.Ctx.__new__(pk_digikit_scen.Ctx)  # parallel() uses no state
    proof = pk_proofkit.Proof.__new__(pk_proofkit.Proof)
    for run in (lambda fns: ctx.parallel(*fns), lambda fns: proof.parallel(*fns)):
        assert peak(run, "1") == 1
        assert peak(run, "2") == 2
        assert peak(run, None) == 4


# ---- the machine: cores, memory, one budget for any machine ------------------------------------------------


def make_cpus(root: Path, cores: int, threads: int, sockets: int = 1) -> None:
    """A fake sysfs: CORES physical cores per socket with THREADS hardware threads each."""
    cpu = 0
    for _ in range(threads):
        for socket in range(sockets):
            for core in range(cores):
                topo = root / f"cpu{cpu}" / "topology"
                topo.mkdir(parents=True)
                (topo / "physical_package_id").write_text(f"{socket}\n")
                (topo / "core_id").write_text(f"{core}\n")
                cpu += 1


def machine(cores: int, ram_gb: float | None) -> verify_budget.Machine:
    return verify_budget.Machine(cores, None if ram_gb is None else int(ram_gb * GB))


@pytest.mark.parametrize(
    ("cores", "ram_gb", "budget", "limit"),
    [
        (16, 128, 14, "cores"),  # Em's M3 Max
        (8, 16, 6, "cores"),  # 16 GB carries 12 emulators: cores bind
        (8, 8, 6, "cores"),
        (4, 16, 2, "cores"),  # 4 vCPU as 4 cores
        (2, 16, 1, "cores"),
        (1, 16, 1, "cores"),
        (32, 16, 24, "memory"),  # (16 - 4) GB at 1 GB per emulator of 2 cores
        (64, 8, 8, "memory"),
        (16, 4, 1, "memory"),  # nothing left after the reserve: one core
        (16, None, 14, "cores"),  # RAM unknown (Windows without ctypes): cores only
    ],
)
def test_budget_is_the_smaller_of_cores_and_memory(
    cores: int, ram_gb: float | None, budget: int, limit: str
) -> None:
    d = verify_budget.decide(machine(cores, ram_gb))
    assert (d.budget, d.limit) == (budget, limit)
    assert d.text.startswith(f"budget {budget} ({limit}")
    assert d.budget >= 1


def test_cargo_jobs_follow_memory() -> None:
    cap = verify_budget.cargo_jobs_cap
    assert cap(128 * GB) == 20  # (128 - 4) / 6
    assert cap(16 * GB) == 2
    assert cap(8 * GB) == 1  # one rustc of the generated core, at most
    assert cap(4 * GB) == 1 and cap(1 * GB) == 1
    assert cap(None) is None
    assert verify_budget.step_env("build", 6, cargo_cap=1)["CARGO_BUILD_JOBS"] == "1"
    assert verify_budget.step_env("build", 6, cargo_cap=20)["CARGO_BUILD_JOBS"] == "6"
    assert (
        verify_budget.step_env("build", 6)["CARGO_BUILD_JOBS"] == "6"
    )  # unknown: uncapped
    assert (
        verify_budget.step_env("python-tests", 6, cargo_cap=1)["CARGO_BUILD_JOBS"]
        == "1"
    )


def test_bounded_env_caps_cargo_by_memory(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(verify_budget, "machine_cargo_cap", lambda: 1)
    assert verify_budget.bounded_env("build", 6, {})["CARGO_BUILD_JOBS"] == "1"


def test_linux_smt_counts_physical_cores(tmp_path: Path) -> None:
    make_cpus(tmp_path, cores=8, threads=2)  # 16 logical, 8 physical
    assert verify_budget.linux_physical_cores(tmp_path, set(range(16))) == 8
    assert verify_budget.decide(machine(8, 64)).budget == 6
    make_cpus(
        tmp_path / "two", cores=4, threads=2, sockets=2
    )  # two sockets: core ids repeat
    assert verify_budget.linux_physical_cores(tmp_path / "two", set(range(16))) == 8
    # affinity: cpus 0-7 are the first thread of each core, 8-15 the second
    assert verify_budget.linux_physical_cores(tmp_path, {0, 1, 2, 3, 8, 9}) == 4
    assert verify_budget.linux_physical_cores(tmp_path / "none", set()) is None


def test_cgroup_cpu_quota(tmp_path: Path) -> None:
    assert verify_budget.parse_cpu_max("max 100000") is None
    assert verify_budget.parse_cpu_max("200000 100000") == 2.0
    assert verify_budget.parse_cpu_max("150000 100000") == 1.5
    assert verify_budget.parse_cpu_max("garbage") is None
    v2 = tmp_path / "v2"
    v2.mkdir()
    (v2 / "cpu.max").write_text("200000 100000\n")
    assert verify_budget.cgroup_cpu_limit(v2) == 2.0
    v1 = tmp_path / "v1" / "cpu"
    v1.mkdir(parents=True)
    (v1 / "cpu.cfs_quota_us").write_text("-1\n")
    (v1 / "cpu.cfs_period_us").write_text("100000\n")
    assert verify_budget.cgroup_cpu_limit(tmp_path / "v1") is None
    (v1 / "cpu.cfs_quota_us").write_text("200000\n")
    assert verify_budget.cgroup_cpu_limit(tmp_path / "v1") == 2.0
    assert verify_budget.cgroup_cpu_limit(tmp_path / "missing") is None
    # a quota of 2 CPUs is 2 cores: budget cores - 2, at least 1
    assert verify_budget.decide(machine(2, 16)).budget == 1


def test_cgroup_memory_limit(tmp_path: Path) -> None:
    (tmp_path / "memory.max").write_text("max\n")
    assert verify_budget.cgroup_memory_limit(tmp_path) is None
    (tmp_path / "memory.max").write_text(str(8 * GB))
    assert verify_budget.cgroup_memory_limit(tmp_path) == 8 * GB
    v1 = tmp_path / "v1" / "memory"
    v1.mkdir(parents=True)
    (v1 / "memory.limit_in_bytes").write_text("9223372036854771712\n")  # "no limit"
    assert verify_budget.cgroup_memory_limit(tmp_path / "v1") is None
    (v1 / "memory.limit_in_bytes").write_text(str(4 * GB))
    assert verify_budget.cgroup_memory_limit(tmp_path / "v1") == 4 * GB


# ---- python-tests: one job per file in the budget queue ----------------------------------------------------------


FILE = verify_budget.FILE_PREFIX


def test_file_jobs_need_their_whole_ask_and_do_not_count_as_steps() -> None:
    budget = verify_budget.Budget(14)
    assert budget.admit("tier-sweep") == 10  # 4 free
    assert budget.admit(FILE + "a.py") == 2
    assert budget.admit(FILE + "b.py") == 2
    assert budget.free == 0 and budget.admit(FILE + "d.py") is None
    budget.release(FILE + "b.py")
    assert budget.admit(FILE + "d.py") == 2
    budget = verify_budget.Budget(5)
    budget.running = {"x": 4}  # one core free: a file job does not start on a sliver
    assert budget.admit(FILE + "a.py") is None
    # many file jobs never hit MAX_STEPS
    budget = verify_budget.Budget(64)
    for i in range(20):
        assert budget.admit(f"{FILE}{i}.py") == 2


def test_files_wait_for_their_builds_and_python_tests_judges_them() -> None:
    nodes, deps = verify.plan(
        verify.EVERYTHING,
        [
            "tests/digikit/test_menu_dt2.py",
            "tests/emulator/test_native_pgo.py",
            "tests/devtools/test_layercheck.py",
        ],
    )
    secs = {
        "tests/digikit/test_menu_dt2.py": 372.0,
        "tests/emulator/test_native_pgo.py": 2.0,
        "tests/devtools/test_layercheck.py": 1.0,
    }
    nodes, deps = verify.expand_python(nodes, deps, list(secs), secs, {})
    daemon, plain, fast = (
        FILE + "tests/digikit/test_menu_dt2.py",
        FILE + "tests/emulator/test_native_pgo.py",
        FILE + "tests/devtools/test_layercheck.py",
    )
    assert deps[daemon] == ("build-daemons",)  # not the DT2 build behind it
    assert deps[plain] == ("build", "build-daemons", "build-dt2")
    assert deps[fast] == ()
    assert set(deps["python-tests"]) == {daemon, plain, fast}
    # the long file starts before the steps, the short one after them, the judge is last
    assert nodes.index(daemon) < nodes.index("tier-quick") < nodes.index(plain)
    assert nodes[-1] == "python-tests"
    nodes, deps = verify.plan(("python-tests",), ["tests/devtools/test_layercheck.py"])
    nodes, deps = verify.expand_python(
        nodes, deps, ["tests/devtools/test_layercheck.py"], {}, {}
    )
    assert deps[fast] == ()  # no daemon build in the plan: nothing to wait for


def test_long_steps_start_with_the_long_files() -> None:
    nodes, deps = verify.plan(
        verify.EVERYTHING,
        ["tests/digikit/test_menu_dt2.py", "tests/devtools/test_layercheck.py"],
    )
    secs = {
        "tests/digikit/test_menu_dt2.py": 372.0,
        "tests/devtools/test_layercheck.py": 2.0,
    }
    daemon = FILE + "tests/digikit/test_menu_dt2.py"
    nodes, _ = verify.expand_python(
        nodes, deps, list(secs), secs, {"runtime-fixture": 4000.0, "smoke": 50.0}
    )
    assert nodes.index("runtime-fixture") < nodes.index(daemon) < nodes.index("smoke")
    nodes, deps = verify.plan(
        verify.EVERYTHING,
        ["tests/digikit/test_menu_dt2.py", "tests/devtools/test_layercheck.py"],
    )
    nodes, _ = verify.expand_python(nodes, deps, list(secs), secs, {})
    assert nodes.index(daemon) < nodes.index(
        "runtime-fixture"
    )  # no estimate: after the files


def test_files_longest_first_and_unknown_files_in_plan_order() -> None:
    from tools.devtools import verify_times

    rows = verify_times.longest_first(["a", "b", "c", "d"], {"a": 5.0, "c": 300.0})
    assert [r[0] for r in rows] == [
        "c",
        "b",
        "d",
        "a",
    ]  # b, d: DEFAULT_SECONDS, plan order
    assert [r[2] for r in rows] == [True, False, False, True]


def test_file_seconds_read_the_newest_runs(tmp_path: Path) -> None:
    from tools.devtools import verify_times

    def run(name: str, tsv: str | None = None) -> None:
        out = tmp_path / name / "python-tests"
        out.mkdir(parents=True)
        if tsv:
            (out / "summary.tsv").write_text("file\tstatus\trc\tseconds\n" + tsv)

    run("20260101-000000", tsv="fa.py\tOK\t0\t10.0\nfb.py\tOK\t0\t20.0\n")
    run(
        "20260102-000000",
        tsv="fa.py\tOK\t0\t30.0\nfb.py\tTIMEOUT\t1\t900.0\n",
    )
    run("20260103-000000")  # a run that did not finish: no summary.tsv
    run("20260104-000000")  # this run: left out
    got = verify_times.file_seconds(
        tmp_path,
        ["fa.py", "fb.py", "fc.py", "fd.py"],
        tmp_path / "20260104-000000",
    )
    assert got == {"fa.py": 30.0, "fb.py": 20.0}


def test_step_seconds_read_the_newest_runs(tmp_path: Path) -> None:
    from tools.devtools import verify_times

    def run(name: str, tsv: str | None = None) -> None:
        (tmp_path / name).mkdir(parents=True)
        if tsv:
            (tmp_path / name / "jobs.tsv").write_text(
                "node\tstart_s\tend_s\tshare\tload_start\tload_end\n" + tsv
            )

    run("20260101-000000", tsv="check\t0.0\t10.0\t2\t1\t1\nbad\n")
    run("20260102-000000", tsv="check\t5.0\t35.0\t2\t1\t1\npins\t1.0\t3.0\t2\t1\t1\n")
    run("20260103-000000")  # no jobs.tsv
    run("20260104-000000", tsv="check\t0.0\t999.0\t2\t1\t1\n")  # this run: left out
    got = verify_times.step_seconds(
        tmp_path, ["check", "pins", "tier-sweep"], tmp_path / "20260104-000000"
    )
    assert got == {"check": 30.0, "pins": 2.0}


def test_file_jobs_leave_the_cores_a_waiting_step_needs() -> None:
    nodes = ["pins", FILE + "x.py"]
    deps: dict[str, tuple[str, ...]] = {n: () for n in nodes}
    budget = verify_budget.Budget(14)
    budget.running = {"tier-sweep": 8, "check": 5}  # 1 free: pins (asks 7) waits
    assert verify.next_starts(nodes, deps, set(), budget) == []
    budget.running = {
        "tier-sweep": 8
    }  # 6 free: pins starts on 6, the file job waits for cores
    got = verify.next_starts(nodes, deps, set(), budget)
    assert got == [("pins", 6)]
    # a file job before the waiting step is not held back
    budget = verify_budget.Budget(14)
    budget.running = {"tier-sweep": 8, "check": 5}
    assert (
        verify.next_starts(
            [FILE + "y.py", "pins"], deps | {FILE + "y.py": ()}, set(), budget
        )
        == []
    )


@pytest.mark.parametrize("node", ["runtime-fixture", "module-load"])
def test_coupled_acceptance_asks_for_a_daemon_per_file_up_to_four(node: str) -> None:
    assert verify_budget.want_cores(node, 16) == 8
    assert verify_budget.want_cores(node, 16, files=3) == 6
    assert verify_budget.want_cores(node, 16, files=1) == 2
    assert verify_budget.want_cores(node, 16, files=9) == 8
    assert verify_budget.want_cores(node, 5) == 5
    assert verify_budget.want_cores(node, 1) == 1


@pytest.mark.parametrize("node", ["runtime-fixture", "module-load"])
def test_runtime_jobs_variable_forces_the_daemon_count(
    node: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("DIGI_RUNTIME_JOBS", "1")
    assert verify_budget.want_cores(node, 16) == 2
    assert verify_budget.want_cores(node, 16, files=4) == 2
    monkeypatch.setenv("DIGI_RUNTIME_JOBS", "3")
    assert verify_budget.want_cores(node, 16, files=1) == 6


@pytest.mark.parametrize("node", ["runtime-fixture", "module-load"])
def test_coupled_acceptance_admits_whole_daemons(node: str) -> None:
    budget = verify_budget.Budget(total=8)
    budget.running["web"] = 7
    assert budget.admit(node) is None  # one core is not a daemon
    budget.release("web")
    budget.running["web"] = 5
    assert budget.admit(node) == 2  # 3 free: one daemon
    budget.release(node)
    budget.release("web")
    budget.running["web"] = 1
    assert budget.admit(node) == 6  # 7 free: three daemons
    budget.release(node)
    budget.release("web")
    assert budget.admit(node) == 8
    budget.release(node)
    budget.files[node] = 2
    assert budget.admit(node) == 4


@pytest.mark.parametrize("steps", [("web",), ("host", "web"), verify.STEPS])
def test_web_plan_requires_host_once(steps: tuple[str, ...]) -> None:
    nodes, deps = verify.plan(steps, [])
    assert nodes.count("host") == 1
    assert deps["web"] == ("host",)
    assert deps["host"] == ()


def test_explicit_host_plan_has_no_build_or_device_prerequisites() -> None:
    assert verify.plan(("host",), []) == (["host"], {"host": ()})
    assert "host" in verify.EVERYTHING
    assert "web" in verify.EVERYTHING
