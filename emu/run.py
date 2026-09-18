# pyright: reportMissingImports=false
# fmt: off
"""Run the emulator against a firmware `.syx`, in one command.

    uv run python -m emu.run [firmware.syx] [--weakptr] [--slc] [--scale N]

Everything between a `.syx` and a live panel, with each prerequisite checked
and built if it can be. There are three, and only the first is yours to find:

  1. the `.syx` itself -- yours, never redistributed here;
  2. `sections/section_3_MAIN_OS.bin`, the decompressed ColdFire image, which
     `emu.extract` produces on first run (about a minute);
  3. a boot snapshot, which this builds for you on first run (a few minutes).
"""
import hashlib
import json
import os
import struct
import subprocess
import sys

from emu import config

TESTED = config.TESTED_NAME
SNAPSHOT = 'boot400M.snap'
LADDER = '60000000,120000000,200000000,280000000,400000000'
MARKER = '.source-sha256'


def marker_path():
    return os.path.join(config.sections_dir(), MARKER)


LADDER_CONFIG = '.ladder.json'


def ladder_config_path(prefix):
    """-> path of the sidecar recording what a ladder was built with.

    A snapshot carries no manifest on the cold-boot path -- snapshot.save is
    called without one there -- so nothing in the .snap file itself records
    whether sdgate/esdhc were installed for the build that produced it. This
    sidecar is what makes a configuration change (e.g. turning storage models
    on) invalidate an existing ladder instead of silently resuming it into a
    mixed state: hooks installed now that were not there when the state was
    captured.
    """
    return os.path.join(os.path.dirname(prefix) or '.', LADDER_CONFIG)


def sha256(path):
    h = hashlib.sha256()
    # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
    with open(path, 'rb') as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def paths_for(syx):
    """-> (snapshot, prefix) for this firmware.

    Sections and snapshots are firmware-specific, and `sections/` can only hold
    one firmware at a time because the filenames are fixed. The tested build
    keeps the historic paths so existing snapshots and every path in the docs
    still work; anything else gets its own directory, so a second firmware can
    never quietly resume the first one's boot.
    """
    if config.is_tested(syx):
        return config.snapshot(SNAPSHOT), config.snapshot('boot')
    stem = os.path.splitext(os.path.basename(syx))[0]
    return (config.snapshot(os.path.join(stem, SNAPSHOT)),
            config.snapshot(os.path.join(stem, 'boot')))


def need_matching_sections(syx, accept=False):
    """Refuse to pair the sections with a firmware they did not come from.

    Nothing in the extracted filenames records which `.syx` produced them, and
    the names are fixed, so the directory holds exactly one firmware at a time.
    Without this a second firmware silently runs against the first one's code.

    When there is no marker the provenance is genuinely unknown, so this does
    NOT guess. For the tested build it records and continues, because that is
    what a pre-existing checkout almost certainly holds; for anything else it
    stops and makes you say so with --accept-sections. An earlier version wrote
    the marker unconditionally and thereby labelled one device's sections with
    another device's hash, which is precisely the failure it exists to prevent.
    """
    digest = sha256(syx)
    path = marker_path()
    if os.path.exists(path):
        # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
        was = open(path).read().strip()
        if was == digest:
            return
        raise SystemExit(
            '%s/ does not belong to %s.\n\n'
            '  sections came from  %s\n'
            '  you asked for       %s\n\n'
            'The section filenames are fixed, so that directory holds only one\n'
            'firmware at a time. Re-extract to switch:\n\n'
            '    rm -rf %s/\n'
            '    uv run python -m emu.extract %s -o %s/'
            % (config.sections_dir(), os.path.basename(syx), was[:16],
               digest[:16], config.sections_dir(), syx, config.sections_dir()))
    if not (config.is_tested(syx) or accept):
        raise SystemExit(
            'Cannot tell which firmware %s/ was extracted from, and you asked\n'
            'for %s, which is not the tested build.\n\n'
            'Those sections are most likely another firmware\'s, and running\n'
            'them would emulate that one under this one\'s name. Either\n'
            're-extract:\n\n'
            '    rm -rf %s/\n'
            '    uv run python -m emu.extract %s -o %s/\n\n'
            'or pass --accept-sections if you are certain they match.'
            % (config.sections_dir(), os.path.basename(syx),
               config.sections_dir(), syx, config.sections_dir()))
    # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
    open(path, 'w').write(digest + '\n')
    print('Recorded %s/ as belonging to %s (%s).\n'
          % (config.sections_dir(), os.path.basename(syx), digest[:16]))


def usable_rung(prefix, default):
    """-> the ladder snapshot the GUI can actually resume the intro from.

    The rungs are fixed instruction counts, and two firmwares do not reach the
    same phase at the same count. At 400M Digitakt's intro is mid-draw; the
    same count on Digitone is already past it, with the intro task parked
    inside `sem_pend` on the frame semaphore. That state cannot be resumed:
    `unblock` only ever sees a pend on the way IN, so it can never satisfy a
    wait that is already blocked, and the GUI holds PIT3 for as long as the
    intro owns vector 208 -- so the one thing that could post the semaphore is
    switched off. The run sits there and the panel stays black.

    So choose by state rather than by number: newest rung first, take the
    first one whose intro is both live and not already parked. Digitakt
    qualifies at every rung and therefore still gets 400M, unchanged.

    The rung a given firmware lands on is an OBSERVATION, not a rule, and it
    moves when the image is relinked. Digitakt II 1.15C gets 400M and Digitone
    II 1.10E gets 280M; Digitone II **1.11** gets 400M again -- on that build
    the 280M rung composes no frame at all and leaves the timers held, so a
    caller who hard-codes 280M "because it is a Digitone" resumes a machine
    that never draws. This function is the answer to that question; call it
    rather than copying a number out of this docstring.

    Falls back to `default` when nothing qualifies -- a firmware whose intro
    this cannot recognise is no worse off than before.
    """
    from emu import symbols
    from emu.pit import intro_running
    from emu.snapshot import restore

    try:
        profile = symbols.resolve(open(config.main_image(), 'rb').read())
    except Exception:                                   # noqa: BLE001
        return default
    if profile.intro_pit3_isr is None or profile.frame_sem is None:
        return default

    # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
    for at in sorted((int(n) for n in LADDER.split(',')), reverse=True):
        path = '%s%dM.snap' % (prefix, at // 1_000_000)
        if not os.path.exists(path):
            continue
        try:
            m, _extra, _regs = restore(path)
            live = intro_running(m, profile.intro_pit3_isr)
            waiter = struct.unpack(
                '>I', m.uc.mem_read(profile.frame_sem + 4, 4))[0]
        except Exception:                               # noqa: BLE001
            continue
        if live and not waiter:
            if path != default:
                print('Resuming from %s rather than %s: at the later rung this\n'
                      "firmware's intro has already parked on the frame "
                      'semaphore,\nwhich cannot be resumed.\n' % (path, default))
            return path
    return default


def need_syx(path):
    if path and os.path.exists(path):
        return
    raise SystemExit(
        'Missing firmware: %s\n\n'
        'No Elektron firmware ships with this repository and none ever should\n'
        '-- it is copyright Elektron. Supply your own lawfully-obtained copy\n'
        'and put it in the working directory. Only %s has been tested.'
        % (path, TESTED))


def need_sections(syx):
    """Decompress the sections if they are not there yet."""
    try:
        config.main_image()
        return
    except config.NotFound:
        pass
    from emu import extract  # imported late: it pulls in Unicorn
    print('No extracted sections yet. Decompressing %s -- about a minute,\n'
          'and only once.\n' % os.path.basename(syx), flush=True)
    for sid, kind, path, n, dest in extract.extract(syx, config.sections_dir()):
        print('  %-26s %9d bytes' % (os.path.basename(path), n), flush=True)
    print()
    config.main_image()          # confirm; raises config.NotFound if not


def need_snapshot(snapshot, prefix, syx, sdgate=True, esdhc=True, main_sha256=None):
    """Build the boot ladder if the snapshot is missing OR stale. -> True if built.

    "Stale" covers more than "absent": a ladder built by an older version of
    this code has no storage models baked in, and resuming it now that
    build()/run() install sdgate/esdhc by default would silently mix an
    unmodelled-storage cold boot with a storage-modelled resume. The sidecar
    written by emu.checkpoint.make (see emu/run.py's ladder_config_path) is
    what makes that detectable: a snapshot itself carries no manifest on the
    cold-boot path (snapshot.save is called without one there), so without
    the sidecar there would be nothing to compare against.
    """
    if os.path.exists(snapshot):
        cfg_path = ladder_config_path(prefix)
        try:
            # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
            with open(cfg_path) as fh:
                cfg = json.load(fh)
        except (OSError, ValueError):
            print('Rebuilding: existing snapshots have no %s sidecar, so '
                  'their storage-model configuration is unknown.\n' % cfg_path,
                  flush=True)
        else:
            if bool(cfg.get('sdgate')) != bool(sdgate):
                print('Rebuilding: existing snapshots were built with '
                      'sdgate=%s, this run wants sdgate=%s.\n'
                      % (cfg.get('sdgate'), sdgate), flush=True)
            elif bool(cfg.get('esdhc')) != bool(esdhc):
                print('Rebuilding: existing snapshots were built with '
                      'esdhc=%s, this run wants esdhc=%s.\n'
                      % (cfg.get('esdhc'), esdhc), flush=True)
            elif main_sha256 is not None and cfg.get('main_sha256') != main_sha256:
                print('Rebuilding: existing snapshots were built from a '
                      'different MAIN OS image.\n', flush=True)
            else:
                return False
    # pi-lens-ignore: ast-grep:unchecked-throwing-call-python
    os.makedirs(os.path.dirname(snapshot) or '.', exist_ok=True)
    # The stale paths above have already printed why they are rebuilding, so
    # only claim the snapshot is absent when it actually is -- otherwise this
    # reads as a contradiction right under "Rebuilding: existing snapshots...".
    if os.path.exists(snapshot):
        print('Rebuilding the boot snapshots -- one cold boot from reset,\n'
              'about 400M instructions, so expect a few minutes.\n', flush=True)
    else:
        print('No %s yet. Building the boot snapshots -- one cold boot from\n'
              'reset, about 400M instructions, so expect a few minutes. This\n'
              'happens once.\n' % snapshot, flush=True)
    r = subprocess.run([sys.executable, '-m', 'emu.checkpoint', 'make',
                        LADDER, prefix, syx,
                        '--sdgate' if sdgate else '--no-sdgate',
                        '--esdhc' if esdhc else '--no-esdhc'])
    if r.returncode != 0 or not os.path.exists(snapshot):
        raise SystemExit('Snapshot build failed; cannot continue.')
    print('\nBuilt %s.\n' % snapshot)
    return True


USAGE = """usage: python -m emu.run [firmware.syx] [snapshot] [options]

  --weakptr    step over the weak_ptr branches that freeze the main task
  --slc        force the eMMC SLC flag (unnecessary with the eSDHC model)
  --scale N    integer panel zoom (default: fits your screen)
  --exact      exact `count=` stepping instead of the default block-bounded
               stepping: 7.6x slower, but every timer lands on the instruction
               it was due at. Use it to compare a run against bootcheck.
  --unthrottled
               run as fast as the host allows, instead of pacing to the
               hardware's own clock
  --accept-sections
               confirm the extracted sections match the firmware you named
  --check      resolve and validate everything, then stop without running
  --help       this

Only %s has been tested.""" % TESTED


def main(argv):
    if '--help' in argv or '-h' in argv:
        print(USAGE)
        return 0
    flags = [a for a in argv if a.startswith('--')]
    rest = [a for a in argv if not a.startswith('--')]
    if '--scale' in argv:                       # --scale takes a value
        i = argv.index('--scale')
        if i + 1 < len(argv) and argv[i + 1] in rest:
            rest.remove(argv[i + 1])
    syx = config.firmware(rest[0] if rest else None)
    default_snap, prefix = paths_for(syx)
    snapshot = rest[1] if len(rest) > 1 else default_snap

    need_sections(syx)
    need_matching_sections(syx, accept='--accept-sections' in flags)
    if not config.is_tested(syx):
        print('Note: only %s has been tested, and every address in this\n'
              'project is specific to that build. A different firmware will\n'
              'very likely not boot. Its snapshots go under %s so they cannot\n'
              'be confused with the tested build\'s.\n' % (TESTED, prefix))
    if '--check' in flags:
        from emu.unicorn_compat import require_compatible_unicorn
        require_compatible_unicorn()
        print('Checks passed. firmware=%s  sections=%s/  snapshot=%s'
              % (syx, config.sections_dir(), snapshot))
        return 0
    need_snapshot(snapshot, prefix, syx, main_sha256=sha256(config.main_image()))
    # Only when the user did not name one: an explicit snapshot is an
    # instruction, not a suggestion.
    if len(rest) <= 1:
        snapshot = usable_rung(prefix, snapshot)

    gui_flags = [f for f in flags
                 if f not in ('--accept-sections', '--check')]
    # `--scale` is the one flag here that takes a value, and the value does
    # not start with `--`, so the split above left it behind in `rest` and
    # then dropped it. emu.gui would receive a bare `--scale` and die reading
    # the argument after it. Put it back where it belongs.
    if '--scale' in gui_flags:
        i = argv.index('--scale')
        if i + 1 < len(argv):
            gui_flags.insert(gui_flags.index('--scale') + 1, argv[i + 1])
    cmd = [sys.executable, '-m', 'emu.gui', snapshot, '--syx', syx] + gui_flags
    print('$ %s\n' % ' '.join(cmd), flush=True)
    return subprocess.call(cmd)


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
