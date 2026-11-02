"""One CPU budget for the whole verify gate (docs/TESTING.md, "CPU budget").

Before this module every step had its own pool (tier sweep cpus - 4 emulator processes, python-tests
one process per file, digikit files 3 daemons each, cargo and libtest on all cores) and up to 8 steps
ran at once, so the pools multiplied: a full run reached load average 141 on a 16-core machine
(2026-10-19). Now verify has a total budget of BUDGET cores and every running step holds a share of it:

* the budget is min(physical cores - 2, the cores the RAM carries), at least 1 (detect_machine, decide;
  DIGI_VERIFY_BUDGET or `verify --jobs N` override it), and cargo jobs are capped by RAM separately;
* a step asks for a fixed fraction of the budget (WANT); the scheduler admits it when that many cores
  (at least half of the ask, never fewer than one) are free, and the step gets what was free, at most
  its ask. The shares of the running steps never add up to more than the budget (class Budget);
* the share sets every inner pool of the step (step_env): CARGO_BUILD_JOBS, RUST_TEST_THREADS,
  RAYON_NUM_THREADS, DIGI_JOBS (scenario renders), DIGI_TIER_JOBS, DIGI_PY_JOBS, DIGIKIT_JOBS, DIGIKIT_PART_JOBS,
  DIGI_CONFORM_JOBS, DIGI_PK_JOBS. A coupled emulator process runs two busy threads (the ColdFire loop
  and the `sharc-dsp` worker of packages/emulator/boot/src/sharc_peer.rs; no environment variable
  changes that), so a pool of emulator processes has share // EMULATOR_CORES workers.

`lower_priority` puts verify and its children at nice 10 (not `taskpolicy -b`, which pins a process to
the efficiency cores of an Apple-silicon machine).
"""

from __future__ import annotations

import functools
import math
import os
import platform
import subprocess
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path

EMULATOR_CORES = 2  # threads one coupled-run / digi-emud process keeps busy
NICE = 10
MAX_STEPS = 6  # steps running at once, whatever the budget

# What a step asks for, as a fraction of the budget (rounded, at least one core). Builds and cargo
# test take the cores that rustc can use (the generated core is one huge crate: more jobs do not
# help); the pools of emulator processes ask for the most.
WANT: dict[str, float] = {
    "build": 0.45,
    "build-dt2": 0.45,
    "build-daemons": 0.45,
    "desktop": 0.15,
    "host": 0.15,
    "stock": 0.25,
    "web": 0.15,
    "rust-tests": 0.30,
    "python-tests": 0.45,
    "card": 0.15,
    "smoke": 0.50,
    "dt2-tone": 0.15,
    "pins": 0.50,
    "check": 0.50,
    "check-no-aot": 0.50,
    "dt2-check": 0.50,
    "tier-quick": 0.75,
    "tier-sweep": 0.75,
    "dt2-tier-quick": 0.75,
    "dt2-tier-sweep": 0.75,
}
DEFAULT_WANT = 0.30
# One python-tests file is a job of its own (verify.py expand_python): it asks for the cores of one
# emulator process pair (its digikit daemon, or the cargo/rayon of a unit test) and, unlike a step, only
# starts on all of them. The jobs do not count against MAX_STEPS: the cores bound them.
FILE_PREFIX = "py:"
BUILDS = ("build", "build-dt2", "build-daemons")
EMULATOR_POOLS = (
    "pins",
    "smoke",
    "check",
    "check-no-aot",
    "dt2-check",
    "tier-quick",
    "tier-sweep",
    "dt2-tier-quick",
    "dt2-tier-sweep",
)


GB = 1024**3
# Memory figures measured in the first full run on a 16-core, 128 GB machine (2026-10-19, load samples in the
# verify run logs): each digi-emud / coupled-run process held 0.5 to 1.0 GB RSS, one rustc compiling the generated
# SHARC core (sharc-gen) about 5.9 GB. Round up.
GB_PER_EMULATOR = 1.0  # RSS of one emulator process (EMULATOR_CORES cores)
GB_PER_CARGO_JOB = 6.0  # RSS of one rustc job while the generated core builds
RESERVE_GB = 4.0  # left to the OS, the editor, the browser


@dataclass(frozen=True)
class Machine:
    """What the gate may use: physical cores (affinity and cgroup quota applied), RAM (cgroup limit applied)."""

    cores: int
    ram: int | None = None  # bytes; None when unknown
    cores_note: str = ""  # which probe answered, for the log


@dataclass(frozen=True)
class Decision:
    budget: int
    limit: str  # "cores" or "memory"
    text: str  # "budget 14 (cores)", "budget 6 (memory: 16 GB)"


def _read(path: Path) -> str | None:
    try:
        return path.read_text().strip()
    except OSError:
        return None


def _int_file(path: Path) -> int | None:
    text = _read(path)
    try:
        return int(text) if text is not None else None
    except ValueError:
        return None


def parse_cpu_max(text: str) -> float | None:
    """cgroup v2 cpu.max ("max 100000" or "200000 100000") as a number of CPUs; None when unlimited."""
    parts = text.split()
    if len(parts) != 2 or parts[0] == "max":
        return None
    try:
        quota, period = int(parts[0]), int(parts[1])
    except ValueError:
        return None
    return quota / period if quota > 0 and period > 0 else None


def cgroup_cpu_limit(root: Path = Path("/sys/fs/cgroup")) -> float | None:
    """The CPU quota of this container in CPUs (cgroup v2 cpu.max, else v1 cpu.cfs_quota_us / period)."""
    v2 = _read(root / "cpu.max")
    if v2 is not None:
        return parse_cpu_max(v2)
    quota = _int_file(root / "cpu/cpu.cfs_quota_us")
    period = _int_file(root / "cpu/cpu.cfs_period_us")
    if quota and period and quota > 0 and period > 0:
        return quota / period
    return None


def cgroup_memory_limit(root: Path = Path("/sys/fs/cgroup")) -> int | None:
    """The memory limit of this container in bytes (v2 memory.max, else v1 limit_in_bytes); None when unlimited."""
    text = _read(root / "memory.max")
    if text is None:
        value = _int_file(root / "memory/memory.limit_in_bytes")
    else:
        value = int(text) if text.isdigit() else None
    return (
        value if value and value < 1 << 60 else None
    )  # v1 reports a huge number for "no limit"


def linux_physical_cores(
    cpu_root: Path = Path("/sys/devices/system/cpu"),
    affinity: set[int] | None = None,
) -> int | None:
    """Unique (package, core) pairs among the CPUs this process may run on; None without sysfs topology."""
    if affinity is None:
        affinity = (
            set(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None
        )
    pairs: set[tuple[int, int]] = set()
    for topo in sorted(cpu_root.glob("cpu[0-9]*/topology")):
        cpu = int(topo.parent.name[3:])
        if affinity is not None and cpu not in affinity:
            continue
        package = _int_file(topo / "physical_package_id")
        core = _int_file(topo / "core_id")
        if core is not None:
            pairs.add((package or 0, core))
    return len(pairs) or None


def lscpu_physical_cores(affinity: set[int] | None = None) -> int | None:
    """The same from `lscpu -p=cpu,core,socket`."""
    try:
        out = subprocess.run(
            ["lscpu", "-p=cpu,core,socket"], capture_output=True, text=True, timeout=5
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    pairs: set[tuple[str, str]] = set()
    for line in out.splitlines():
        fields = line.split(",")
        if line.startswith("#") or len(fields) < 3:
            continue
        if (
            affinity is not None
            and fields[0].isdigit()
            and int(fields[0]) not in affinity
        ):
            continue
        pairs.add((fields[2], fields[1]))
    return len(pairs) or None


def _sysctl(name: str) -> int | None:
    try:
        out = subprocess.run(
            ["sysctl", "-n", name], capture_output=True, text=True, timeout=5
        ).stdout.strip()
        return int(out)
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def _windows_ram() -> int | None:
    try:
        import ctypes

        class Status(ctypes.Structure):
            _fields_ = [
                ("length", ctypes.c_ulong),
                ("load", ctypes.c_ulong),
                ("total", ctypes.c_ulonglong),
                ("avail", ctypes.c_ulonglong),
                ("total_page", ctypes.c_ulonglong),
                ("avail_page", ctypes.c_ulonglong),
                ("total_virtual", ctypes.c_ulonglong),
                ("avail_virtual", ctypes.c_ulonglong),
                ("ext", ctypes.c_ulonglong),
            ]

        status = Status()
        status.length = ctypes.sizeof(Status)
        ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(status))  # type: ignore[attr-defined]
        return int(status.total) or None
    except (OSError, AttributeError, ImportError):
        return None


def _linux_ram() -> int | None:
    text = _read(Path("/proc/meminfo")) or ""
    for line in text.splitlines():
        if line.startswith("MemTotal:"):
            return int(line.split()[1]) * 1024
    return None


def detect_machine() -> Machine:
    """Probe this machine. Logical counts include SMT threads, so a fallback halves them."""
    logical = os.cpu_count() or 2
    system = platform.system()
    ram: int | None
    if system == "Darwin":
        phys = _sysctl("hw.physicalcpu")
        ram = _sysctl("hw.memsize")
        if phys:
            return Machine(max(phys, 1), ram, "sysctl hw.physicalcpu")
        return Machine(max(logical // 2, 1), ram, "cpu_count // 2")
    if system == "Linux":
        affinity = (
            set(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None
        )
        usable = len(affinity) if affinity else logical
        cores, note = linux_physical_cores(affinity=affinity), "sysfs topology"
        if cores is None:
            cores, note = lscpu_physical_cores(affinity), "lscpu"
        if cores is None:
            cores, note = max(usable // 2, 1), "cpu_count // 2"
        quota = cgroup_cpu_limit()
        if quota is not None and math.ceil(quota) < cores:
            cores, note = (
                max(math.ceil(quota), 1),
                f"{note}, cgroup quota {quota:g} CPUs",
            )
        ram = _linux_ram()
        limit = cgroup_memory_limit()
        if limit is not None and (ram is None or limit < ram):
            ram = limit
        return Machine(cores, ram, note)
    ram = _windows_ram() if system == "Windows" else None
    return Machine(max(logical // 2, 1), ram, "cpu_count // 2")


def physical_cores() -> int:
    return detect_machine().cores


def memory_cores(ram: int | None) -> int | None:
    """The cores that RAM carries: (RAM - reserve) at GB_PER_EMULATOR per EMULATOR_CORES cores; None if unknown."""
    if ram is None:
        return None
    usable = ram / GB - RESERVE_GB
    return max(1, int(usable / GB_PER_EMULATOR) * EMULATOR_CORES)


def cargo_jobs_cap(ram: int | None) -> int | None:
    """rustc jobs that fit in RAM while the generated core builds; None when RAM is unknown."""
    if ram is None:
        return None
    return max(1, int((ram / GB - RESERVE_GB) / GB_PER_CARGO_JOB))


def decide(machine: Machine) -> Decision:
    """min(physical cores - 2, cores that the RAM carries), at least 1, and which of the two applied."""
    by_cores = max(1, machine.cores - 2)
    by_memory = memory_cores(machine.ram)
    if by_memory is not None and by_memory < by_cores:
        gb = machine.ram / GB if machine.ram else 0
        return Decision(
            by_memory, "memory", f"budget {by_memory} (memory: {gb:.0f} GB)"
        )
    return Decision(
        by_cores, "cores", f"budget {by_cores} (cores: {machine.cores} physical)"
    )


def default_budget(cores: int | None = None) -> int:
    """Physical cores - 2, at least 1 (no memory limit); without CORES, the decision for this machine."""
    if cores is not None:
        return max(1, cores - 2)
    return decide(detect_machine()).budget


@functools.lru_cache(maxsize=1)
def machine_cargo_cap() -> int | None:
    return cargo_jobs_cap(detect_machine().ram)


def budget_text(jobs: int = 0, environ: Mapping[str, str] | None = None) -> str:
    """The line that says which limit applied: "budget 14 (cores: 16 physical)", "budget 6 (--jobs)"."""
    environ = os.environ if environ is None else environ
    if jobs:
        return f"budget {jobs} (--jobs)"
    if environ.get("DIGI_VERIFY_BUDGET"):
        return f"budget {environ['DIGI_VERIFY_BUDGET']} (DIGI_VERIFY_BUDGET)"
    machine = detect_machine()
    cap = cargo_jobs_cap(machine.ram)
    extra = f"; cargo jobs at most {cap}" if cap is not None else ""
    return f"{decide(machine).text}{extra}"


def resolve_budget(jobs: int = 0, environ: Mapping[str, str] | None = None) -> int:
    """The total budget: `--jobs N`, else DIGI_VERIFY_BUDGET, else the default."""
    environ = os.environ if environ is None else environ
    if jobs:
        if jobs < 1:
            raise SystemExit("verify: --jobs must be at least 1")
        return jobs
    env = environ.get("DIGI_VERIFY_BUDGET")
    if env:
        if not env.isdigit() or int(env) < 1:
            raise SystemExit("verify: DIGI_VERIFY_BUDGET must be an integer >= 1")
        return int(env)
    return default_budget()


def lower_priority(foreground: bool = False) -> int | None:
    """Set this process (and so every child) to nice NICE unless FOREGROUND; the new nice value,
    None when left alone. A process already niced more is not made more favoured."""
    if foreground:
        return None
    try:
        current = os.getpriority(os.PRIO_PROCESS, 0)
        if current >= NICE:
            return current
        os.setpriority(os.PRIO_PROCESS, 0, NICE)
        return NICE
    except OSError:
        return None


def is_file_job(node: str) -> bool:
    return node.startswith(FILE_PREFIX)


COUPLED_ACCEPTANCE = ("runtime-fixture", "module-load")
MAX_RUNTIME_JOBS = (
    4  # Vitest workers (one coupled daemon each) of a runtime or conformance step
)


def runtime_jobs(files: int | None = None) -> int:
    """The Vitest workers of a runtime or conformance step: DIGI_RUNTIME_JOBS, else min(MAX_RUNTIME_JOBS,
    FILES) (FILES None: not known, so MAX_RUNTIME_JOBS)."""
    env = os.environ.get("DIGI_RUNTIME_JOBS")
    if env:
        if not env.isdigit() or int(env) < 1:
            raise SystemExit("verify: DIGI_RUNTIME_JOBS must be an integer >= 1")
        return int(env)
    return max(1, min(MAX_RUNTIME_JOBS, files or MAX_RUNTIME_JOBS))


def want_cores(node: str, total: int, files: int | None = None) -> int:
    """The cores NODE asks for in a budget of TOTAL (an emulator pool asks for at least one process).
    FILES is the number of test files of a runtime or conformance step, when known."""
    if node in COUPLED_ACCEPTANCE:
        return min(EMULATOR_CORES * runtime_jobs(files), total)
    if is_file_job(node):
        return min(EMULATOR_CORES, total)
    cores = max(1, round(WANT.get(node, DEFAULT_WANT) * total))
    if node in EMULATOR_POOLS:
        cores = max(cores, min(EMULATOR_CORES, total))
    return min(cores, total)


@dataclass
class Budget:
    """The cores of the running steps; `admit` never lets them add up to more than `total`."""

    total: int
    max_steps: int = MAX_STEPS
    running: dict[str, int] = field(default_factory=dict)
    files: dict[str, int] = field(
        default_factory=dict
    )  # test files of the steps that know them

    @property
    def free(self) -> int:
        return self.total - sum(self.running.values())

    def admit(self, node: str) -> int | None:
        """The share NODE gets (and holds until `release`), or None when it must wait: fewer than
        half of its ask is free (a step may start on less than the ask, not on a sliver of it), or
        MAX_STEPS steps run already. Alone, a step always starts. A file job (FILE_PREFIX) starts only
        on its whole ask and is not counted in MAX_STEPS."""
        if node in self.running:
            raise ValueError(f"{node} is running already")
        want = want_cores(node, self.total, self.files.get(node))
        share = min(want, self.free)
        coupled = node in COUPLED_ACCEPTANCE
        if coupled:
            if share < min(EMULATOR_CORES, want):
                return None  # one coupled daemon cannot run on less than its cores
            share -= share % EMULATOR_CORES if share >= EMULATOR_CORES else 0
        if is_file_job(node):
            if share < want:
                return None
        else:
            steps = sum(1 for n in self.running if not is_file_job(n))
            if share < 1 or steps >= self.max_steps:
                return None
            if self.running and not coupled and share < math.ceil(want / 2):
                return None
        self.running[node] = share
        return share

    def release(self, node: str) -> None:
        self.running.pop(node, None)


def wave_plan(
    nodes: list[str], deps: dict[str, tuple[str, ...]], total: int
) -> list[list[tuple[str, int]]]:
    """The shares in waves for `--explain`: each wave admits, in order, the nodes whose dependencies
    ended in an earlier wave, and all of it ends before the next wave starts. A real run starts a
    node as soon as cores are free, so it overlaps waves; the shares it hands out obey the same rule."""
    done: set[str] = set()
    pending = list(nodes)
    waves: list[list[tuple[str, int]]] = []
    while pending:
        budget = Budget(total)
        wave: list[tuple[str, int]] = []
        for node in list(pending):
            if all(d in done for d in deps[node]):
                share = budget.admit(node)
                if share:
                    wave.append((node, share))
                    pending.remove(node)
        if not wave:
            break
        done.update(n for n, _ in wave)
        waves.append(wave)
    return waves


@dataclass(frozen=True)
class Workers:
    """The inner pools of one step, from its share."""

    share: int
    cargo: int  # CARGO_BUILD_JOBS (rustc processes)
    libtest: int  # RUST_TEST_THREADS
    rayon: int  # RAYON_NUM_THREADS (ruff, oxlint, ...)
    emulators: int  # coupled-run processes at once: DIGI_JOBS, DIGI_TIER_JOBS
    py_files: int  # test files at once: DIGI_PY_JOBS
    per_file: int  # daemons inside one test file: DIGIKIT_JOBS, DIGI_CONFORM_JOBS, DIGI_PK_JOBS
    node: int  # node --test-concurrency


def workers_for(node: str, share: int) -> Workers:
    share = max(1, share)
    emulators = max(1, share // EMULATOR_CORES)
    if node == "python-tests" or is_file_job(node):
        # `py_files` test files at once; each file holds at most `share // py_files` cores, which
        # its cargo and rayon use or its daemons do (two threads each)
        py_files = max(1, share // EMULATOR_CORES)
        own = max(1, share // py_files)
        return Workers(
            share=share,
            cargo=own,
            libtest=own,
            rayon=own,
            emulators=max(1, own // EMULATOR_CORES),
            py_files=py_files,
            per_file=max(1, own // EMULATOR_CORES),
            node=own,
        )
    return Workers(
        share=share,
        cargo=share,
        libtest=emulators,
        rayon=share,
        emulators=emulators,
        py_files=emulators,
        per_file=1,
        node=share,
    )


def step_env(node: str, share: int, cargo_cap: int | None = None) -> dict[str, str]:
    """The environment variables that bound every pool of NODE to its SHARE (cargo jobs also to CARGO_CAP)."""
    w = workers_for(node, share)
    cargo = w.cargo if cargo_cap is None else max(1, min(w.cargo, cargo_cap))
    return {
        "CARGO_BUILD_JOBS": str(cargo),
        "RUST_TEST_THREADS": str(w.libtest),
        # cargo-nextest reads this, not RUST_TEST_THREADS
        "NEXTEST_TEST_THREADS": str(w.libtest),
        "RAYON_NUM_THREADS": str(w.rayon),
        "DIGI_JOBS": str(w.emulators),
        "DIGI_TIER_JOBS": str(w.emulators),
        "DIGI_PY_JOBS": str(w.py_files),
        "DIGIKIT_JOBS": str(w.per_file),
        # the parts of one digikit scenario (2 to 4 daemons side by side, measured) run one at a time:
        # per_file counts the daemons of a file, whatever their parts
        "DIGIKIT_PART_JOBS": "1",
        "DIGI_CONFORM_JOBS": str(w.per_file),
        "DIGI_PK_JOBS": str(w.per_file),
    }


def bounded_env(
    node: str, share: int, base: dict[str, str] | None = None
) -> dict[str, str]:
    """BASE (default os.environ) with the pool bounds of NODE for SHARE cores. A variable the user set in
    the environment of the gate is not replaced: it overrides the budget (verify prints which)."""
    out = dict(os.environ if base is None else base)
    for key, value in step_env(node, share, machine_cargo_cap()).items():
        if key not in os.environ:
            out[key] = value
    return out


def user_overrides() -> list[str]:
    """The pool variables the user set, which replace the share of the budget."""
    return [key for key in step_env("", 1) if key in os.environ]


# Pools the step does not read are left out of the printed line.
SHOWN: dict[str, tuple[str, ...]] = {
    "python-tests": ("py_files", "per_file", "cargo"),
    "rust-tests": ("cargo", "libtest"),
    "desktop": ("cargo", "node"),
    "host": ("node",),
    "stock": ("node",),
    "runtime-fixture": ("emulators",),
    "module-load": ("emulators",),
    "web": ("cargo", "node"),
    **{n: ("cargo",) for n in BUILDS},
    **{n: ("emulators",) for n in EMULATOR_POOLS},
}
LABELS = {
    "py_files": "files",
    "per_file": "daemons/file",
    "cargo": "cargo jobs",
    "libtest": "test threads",
    "node": "node",
    "emulators": "emulator processes",
}


def describe(node: str, share: int) -> str:
    w = workers_for(node, share)
    shown = SHOWN["python-tests"] if is_file_job(node) else SHOWN.get(node, ("cargo",))
    parts = [f"{LABELS[k]} {getattr(w, k)}" for k in shown]
    return ", ".join(parts)
