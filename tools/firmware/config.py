"""Where the firmware, its sections and the snapshots live.

Nothing in this project should hardcode a firmware filename or directory. It did, in
thirteen files, and the cost was concrete: `unicorn-reference-final:emu/run.py` accepted a `.syx`
argument, passed it to nothing, and cheerfully ran a *different* firmware's
image and snapshots while printing the name you asked for. A hardcoded default
that silently wins over an explicit argument is worse than no default.

Layout (all relative to the repository root, never the working directory):

    firmware/                  your own .syx files (see firmware/README.md)
    out/sections/<image-key>/  extracted sections of one image, e.g. dt2-1.16
    out/snapshots/             boot snapshots

`stock(name)` is the path of a named image in `firmware/`. `image_key(syx)`
names an image by its device file (`devices/*.toml`): `<short>-<version>`.

Every path here resolves in the same order:

    1. an explicit argument, if the caller passed one
    2. the environment variable
    3. discovery in `firmware/`
    4. an error that says what it looked for and how to tell it

Environment variables:

    DT2_SYX        the firmware .syx
    DT2_SECTIONS   the directory of extracted sections   (default: out/sections/<image-key>)
    DT2_SNAPSHOTS  the directory of boot snapshots        (default: out/snapshots)
    DT2_MAIN_IMG   the decompressed MAIN OS image

Discovery is by pattern, not by literal name, so another Elektron device's
sections work without editing anything -- `section_3_MAIN_OS.bin` is one
device's naming, not a law.
"""

import glob
import os
import pathlib

REPO = pathlib.Path(__file__).resolve().parents[2]
# $DIGI_FIRMWARE_DIR overrides it, as it does for the control daemon (packages/emulator/control, Loaded::find_base)
FIRMWARE_DIR = pathlib.Path(os.environ.get("DIGI_FIRMWARE_DIR") or REPO / "firmware")
SECTIONS_ROOT = REPO / "out" / "sections"
SNAPSHOTS_DEFAULT = REPO / "out" / "snapshots"

# The build this project was reverse-engineered against. It breaks a tie when
# several firmware files are present, and it never overrides an explicit
# argument or DT2_SYX.
TESTED_SYX = "Digitakt_II_OS1.15C.syx"
TESTED_NAME = "Digitakt II OS 1.15C"


class NotFound(SystemExit):
    """A path could not be resolved. The message says how to resolve it."""


def stock(name: str) -> pathlib.Path:
    """-> the path of a named image in firmware/ (it may not exist)."""
    return FIRMWARE_DIR / name


# The firmware files of the current analysis images, by image key (AGENTS.md: DT2 OS 1.17, DN2 OS 1.12).
DEVICE_SYX = {
    "dt2-1.17": "Digitakt_II_OS1.17.syx",
    "dn2-1.12": "Digitone_II_OS1.12.syx",
}


def image_key(syx) -> str:
    """-> "<device short>-<version>" (dt2-1.16, dn2-1.10E) of a firmware file.

    Identified by content hash through devices/*.toml. A file that is not
    there is keyed by the filename hint in the device files, so paths can be
    computed before the image is present. An image no device file knows (for
    example another product) is keyed by its file stem.
    """
    from tools.firmware import device

    devdir = str(REPO / "devices")
    try:
        dev, fw = device.identify(str(syx), devdir)
    except OSError:
        name = pathlib.Path(syx).name
        for d in device.load_all(devdir):
            for f in d.firmwares:
                if f.filename == name:
                    return "%s-%s" % (d.short, f.version)
        return pathlib.Path(syx).stem
    except SystemExit:
        return pathlib.Path(syx).stem
    return "%s-%s" % (dev.short, fw.version)


def sections_for(syx) -> pathlib.Path:
    """-> the directory of extracted sections of one image."""
    return SECTIONS_ROOT / image_key(syx)


def sections_dir(syx=None):
    env = os.environ.get("DT2_SECTIONS")
    if env:
        return env
    return str(sections_for(syx or firmware()))


def snapshots_dir():
    return os.environ.get("DT2_SNAPSHOTS", str(SNAPSHOTS_DEFAULT))


def firmware(explicit=None):
    """-> the .syx path.

    An explicit argument wins, then DT2_SYX, then the only `.syx` in firmware/.
    With several present the tested build breaks the tie -- every address in
    this project targets it, so that is a stated preference rather than a
    guess -- and several with none of them tested is a refusal. What must
    never happen is a default quietly overriding something you named.
    """
    if explicit:
        if not os.path.exists(explicit):
            raise NotFound("No such firmware file: %s" % explicit)
        return explicit
    env = os.environ.get("DT2_SYX")
    if env:
        if not os.path.exists(env):
            raise NotFound("DT2_SYX points at a missing file: %s" % env)
        return env
    found = [str(p) for p in sorted(FIRMWARE_DIR.glob("*.syx"))]
    if len(found) == 1:
        return found[0]
    # Several present: prefer the build this project is keyed to, which keeps
    # the many commands that have no firmware argument working. Only a real tie --
    # several, none of them tested -- refuses.
    if str(stock(TESTED_SYX)) in found:
        return str(stock(TESTED_SYX))
    if not found:
        raise NotFound(
            "No firmware found.\n\n"
            "No Elektron firmware ships with this repository and none ever\n"
            "should -- it is copyright Elektron. Put your own lawfully-obtained\n"
            ".syx in %s (see firmware/README.md), pass it as an argument,\n"
            "or set DT2_SYX. Only %s has been tested." % (FIRMWARE_DIR, TESTED_NAME)
        )
    raise NotFound(
        "Several firmware files here, so which one is ambiguous:\n\n%s\n\n"
        "Pass one as an argument or set DT2_SYX. This is deliberately not\n"
        "guessed: picking the wrong one runs one firmware under another's\n"
        "name." % "\n".join("    " + f for f in found)
    )


def main_image(explicit=None):
    """-> the decompressed MAIN OS section, found by pattern not by name."""
    if explicit:
        if not os.path.exists(explicit):
            raise NotFound("No such MAIN OS image: %s" % explicit)
        return explicit
    env = os.environ.get("DT2_MAIN_IMG")
    if env:
        if not os.path.exists(env):
            raise NotFound("DT2_MAIN_IMG points at a missing file: %s" % env)
        return env
    d = sections_dir()
    found = sorted(glob.glob(os.path.join(d, "*MAIN_OS*.bin")))
    if len(found) == 1:
        return found[0]
    if not found:
        raise NotFound(
            "No MAIN OS image in %s/.\n\n"
            "The sections inside a .syx are compressed. Decompress them once:\n\n"
            "    uv run python -m tools.firmware.extract firmware/<image>.syx -o %s/\n\n"
            "Or set DT2_MAIN_IMG to an image you already have." % (d, d)
        )
    raise NotFound(
        "Several MAIN OS images in %s/:\n%s\n\nSet DT2_MAIN_IMG."
        % (d, "\n".join("    " + f for f in found))
    )


def bootstrap(explicit=None):
    """-> the bootstrap section (section 2), which holds the acceptance code."""
    if explicit:
        if not os.path.exists(explicit):
            raise NotFound("No such bootstrap image: %s" % explicit)
        return explicit
    d = sections_dir()
    found = sorted(glob.glob(os.path.join(d, "section_2_*.bin")))
    if found:
        return found[0]
    raise NotFound(
        "No section 2 (bootstrap) in %s/. Extract the sections "
        "first; see main_image()." % d
    )


def snapshot(name):
    """-> a path inside the snapshot directory."""
    return os.path.join(snapshots_dir(), name)


def is_tested(syx):
    return os.path.basename(syx) == TESTED_SYX
