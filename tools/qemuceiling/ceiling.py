#!/usr/bin/env python3
"""QEMU vs. Unicorn ColdFire ceiling, for HANDOVER-2026-09-22's Task A step 4
and the "Decision rule" in the same handover: is Unicorn's raw ceiling well
below real time, and is QEMU's well above it? `docs/findings/07-emulator.md`
already has one side of that: a hook-free Unicorn loop on this machine runs
at 250.8M instructions/s, close to real ColdFire hardware (200-264M/s). This
script gets the other side -- QEMU's ceiling on the same machine -- without
porting anything.

Method: a two-instruction ColdFire loop (tools/qemuceiling/loopimg.py),
hand-encoded once and run byte-for-byte unmodified on both engines from the
same non-zero load address:

    ADDQ.L #1,D0     ;  BRA.s <self>

Neither engine is asked to COUNT instructions -- that is the mistake the
untrusted out/speed-ab/report.md A/B made (`count=` costs ~7.6x by itself,
per docs/findings/07-emulator.md). Instead, both sides run for a wall-clock
budget with no per-instruction accounting, and the instruction count is
recovered afterwards from D0: it is exactly the number of loop iterations,
known by construction (2 instructions/iteration) rather than measured.

  * Unicorn: `emu_start(pc, 0, timeout=<microseconds>)`, no hooks, no
    `count=`. `timeout` bounds wall time without walking every instruction,
    unlike `count=`.
  * QEMU: `qemu-system-m68k -M mcf5208evb -kernel <elf>`, started with `-S`
    (CPU stopped) so boot time is excluded, then driven from the human
    monitor over a unix socket: `cont`, sleep for the budget, `stop`,
    `info registers` to read D0 back, `quit`. The measured `dt` runs from
    just before sending `cont` to just before sending `stop`, so it is the
    intended window plus a few milliseconds of monitor round-trip -- noise
    against a multi-second budget, not startup.

Rules this script follows (HANDOVER-2026-09-22, "Speed A/Bs must do the same
work"; docs/findings/07-emulator.md):
  * No `count=` / no instruction-counted stepping on either side.
  * Same instruction stream on both sides, by construction, not by
    measurement.
  * A short run here is a FUNCTIONAL CHECK, not a timing result. Run
    `--seconds` small (1-2) to prove the tool works; real numbers come later
    from a longer run on a quiet machine, per this task's instructions.

What this ceiling is NOT: a firmware workload. QEMU's TCG (and Unicorn's own
JIT) can keep a two-instruction self-branch entirely in the host's icache and
branch predictor in a way a large, branchy, MMIO-touching firmware image
cannot -- so treat a gap here as an upper bound on QEMU's advantage, not a
prediction of what a ported emulator would see. That is exactly why
docs/findings/07-emulator.md's decision rule asks for BOTH this ceiling AND
the crossings/profile work in tools/speedab.py: this script alone cannot
settle whether QEMU is worth porting to.

Requires `qemu-system-m68k` (Em installed it via `brew install qemu`; this
script checks `shutil.which` and reports its path/version, it does not
install anything).

Examples:
    uv run python tools/qemuceiling/ceiling.py --seconds 1
    uv run python tools/qemuceiling/ceiling.py --seconds 2 --json out/qemuceiling.json
"""
import argparse
import json
import os
import re
import select
import shutil
import socket
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import loopimg  # noqa: E402


class Monitor:
    """A tiny QEMU human-monitor client over a unix socket. No QMP: the
    human monitor's `info registers` is plain text, and `re.search` on it is
    simpler than JSON-mode's structured-but-differently-shaped register
    dump for this one field.
    """

    def __init__(self, path, connect_timeout=5.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        deadline = time.monotonic() + connect_timeout
        while True:
            try:
                self.sock.connect(path)
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if time.monotonic() > deadline:
                    raise RuntimeError(
                        'qemu monitor socket %r never appeared' % path)
                time.sleep(0.05)
        self._drain()  # the startup banner and first "(qemu) " prompt

    def _drain(self, quiet=0.15, max_wait=2.0):
        buf = b''
        deadline = time.monotonic() + max_wait
        while time.monotonic() < deadline:
            r, _, _ = select.select([self.sock], [], [], quiet)
            if not r:
                break
            chunk = self.sock.recv(65536)
            if not chunk:
                break
            buf += chunk
        return buf.decode('latin1', errors='replace')

    def send(self, text):
        """Write a command with no drain -- for the two timing-critical
        commands (`cont`, `stop`), so the caller's timestamp is not pushed
        later by waiting on a reply that arrives after the CPU has already
        started or stopped. Drain separately, after capturing the timestamp."""
        self.sock.sendall((text + '\n').encode())

    def cmd(self, text):
        self.send(text)
        return self._drain()

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def qemu_binary_info(qemu_binary):
    path = shutil.which(qemu_binary)
    if path is None:
        return None, None
    out = subprocess.run([path, '--version'], capture_output=True, text=True,
                         check=False).stdout.strip().splitlines()
    return path, (out[0] if out else '')


def run_qemu(seconds, qemu_binary='qemu-system-m68k'):
    """-> (instrs, dt_seconds, instrs_per_sec). Raises RuntimeError if the
    binary is missing or D0 cannot be recovered."""
    path, _ = qemu_binary_info(qemu_binary)
    if path is None:
        raise RuntimeError('%r not found on PATH' % qemu_binary)

    elf = loopimg.make_elf()
    with tempfile.TemporaryDirectory(prefix='qemuceiling-') as d:
        elf_path = os.path.join(d, 'loop.elf')
        sock_path = os.path.join(d, 'mon.sock')
        with open(elf_path, 'wb') as fh:
            fh.write(elf)

        proc = subprocess.Popen(
            [path, '-M', 'mcf5208evb', '-kernel', elf_path,
             '-nographic', '-no-reboot', '-S',
             '-monitor', 'unix:%s,server,nowait' % sock_path,
             '-serial', 'none', '-display', 'none'],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            mon = Monitor(sock_path)
            try:
                t0 = time.perf_counter()
                mon.send('cont')
                mon._drain()          # not timed: the CPU is already running
                time.sleep(seconds)
                mon.send('stop')
                t1 = time.perf_counter()
                mon._drain()          # not timed: CPU already stopped by t1
                out = mon.cmd('info registers')
                m = re.search(r'D0\s*=\s*([0-9a-fA-F]+)', out)
                if not m:
                    raise RuntimeError(
                        'could not find D0 in `info registers` output: %r' % out)
                d0 = int(m.group(1), 16)
                mon.cmd('quit')
            finally:
                mon.close()
        finally:
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=3)

    dt = t1 - t0
    instrs = d0 * loopimg.INSTRS_PER_ITER
    return instrs, dt, (instrs / dt if dt > 0 else float('nan'))


def run_unicorn(seconds):
    """-> (instrs, dt_seconds, instrs_per_sec). No hooks, no `count=`."""
    from unicorn import Uc, UC_ARCH_M68K, UC_MODE_BIG_ENDIAN
    from unicorn.m68k_const import UC_M68K_REG_D0

    uc = Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)
    uc.mem_map(0, 0x10000)
    uc.mem_write(loopimg.LOAD_ADDR, loopimg.CODE)
    t0 = time.perf_counter()
    uc.emu_start(loopimg.LOAD_ADDR, 0, timeout=int(seconds * 1_000_000))
    dt = time.perf_counter() - t0
    d0 = uc.reg_read(UC_M68K_REG_D0)
    instrs = d0 * loopimg.INSTRS_PER_ITER
    return instrs, dt, (instrs / dt if dt > 0 else float('nan'))


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--seconds', type=float, default=1.0,
                    help='wall-clock budget per side (default: 1.0; keep this '
                         'small for a functional check, per this task\'s rules)')
    ap.add_argument('--qemu-binary', default='qemu-system-m68k')
    ap.add_argument('--json', help='also write the result dict here')
    args = ap.parse_args()

    print(__doc__.splitlines()[0])
    print('NOTE: this is a functional check, not a timing result, unless '
          'you were explicitly asked for numbers on a quiet machine.\n')

    path, version = qemu_binary_info(args.qemu_binary)
    print('qemu binary: %s (%s)' % (path, version) if path else
          'qemu binary: NOT FOUND (%r) -- skipping the QEMU side' % args.qemu_binary)

    result = {'seconds': args.seconds, 'load_addr': loopimg.LOAD_ADDR,
             'instrs_per_iter': loopimg.INSTRS_PER_ITER}

    u_instrs, u_dt, u_ips = run_unicorn(args.seconds)
    print('unicorn : %13d instrs in %.4fs  -> %.4g instr/s'
          % (u_instrs, u_dt, u_ips))
    result['unicorn'] = {'instrs': u_instrs, 'dt': u_dt, 'ips': u_ips}

    if path is not None:
        try:
            q_instrs, q_dt, q_ips = run_qemu(args.seconds, args.qemu_binary)
        except RuntimeError as e:
            print('qemu    : FAILED -- %s' % e)
            result['qemu'] = {'error': str(e)}
        else:
            print('qemu    : %13d instrs in %.4fs  -> %.4g instr/s'
                  % (q_instrs, q_dt, q_ips))
            print('ratio qemu/unicorn: %.3gx (a hot two-instruction loop; see '
                  'the module docstring before drawing any conclusion from this)'
                  % (q_ips / u_ips if u_ips else float('nan')))
            result['qemu'] = {'instrs': q_instrs, 'dt': q_dt, 'ips': q_ips}

    if args.json:
        with open(args.json, 'w') as fh:
            json.dump(result, fh, indent=2)
        print('\nwrote %s' % args.json)


if __name__ == '__main__':
    main()
