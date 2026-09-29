#!/usr/bin/env python3
"""Check a +Drive image built by tools/plusdrive.py against DT2 1.16's own
code, with bounded direct calls (no boot).

The machine is a restored 1.16 snapshot (heap, RTOS objects and the Project
object exist there), with the card served straight from the image file:

* ``FUN_4012deda`` (eMMC read) and ``FUN_4012e0c0`` (eMMC write) are replaced
  by host copies from the image; writes go to an in-memory overlay, so the
  image file is never modified.
* The two mutex types (``FUN_400015a0``/``FUN_400016d2``,
  ``FUN_40001608``/``FUN_4000172a``) are no-ops: each call runs alone, with
  interrupts masked.
* ``FUN_400cd638`` (one 4 KiB page, or a slot header, to the DSP over
  FlexBus) and ``FUN_40153622`` (post a loader event) are recorded instead
  of run.

Checks, in order (``run_checks``):

1. mount ``FUN_4015a450(0)`` and the RAM state it builds: mount flag, id
   bitmap (``FUN_4015b93e``), hash index (``FUN_4015a94c``);
2. ``FUN_4015b178(ref, &id)`` (resolve the sample reference) and
   ``FUN_4015abe6(id, key)`` -> ``FUN_4015ab5c`` (the loader's key, which
   needs record +0x0C bit 0);
3. ``FUN_4015af0c(id)``, the firmware recomputing the content hash from the
   file: the hash-table word it writes must equal the one the image holds;
4. the project record: ``FUN_400efc8e`` reads it from sector 0x40000 into
   0x4099d588, ``FUN_400c0e90`` checks the COKi header, ``FUN_400c0c2c(0)``
   decodes the container into the live Project (``_DAT_4099d584`` stays 0
   unless it had to build a new project), then ``FUN_4004e598`` lists the
   slots "Load all samples" would load, starting with the active kit's
   tracks, and each slot's reference is read from ProjectSettings; the
   active kit's 16 tracks give their machine type (byte +0xd6: 0 ONESHOT,
   1 WERP, 2 STRETCH, 3 REPITCH, 4 GRID, 5 MIDI, 6 SLICE) and sample slot
   (int16 +0x80), as ``FUN_4004e598`` reads them;
5. ``FUN_40154540(ref, slot)`` (sample_loader_load_sample): the FlexBus
   pages and slot header it sends.

    DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/plusdrive_check.py \\
        out/plusdrive/lane-native/dt2.img
"""

import argparse
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from unicorn import UC_HOOK_CODE, UC_HOOK_INTR  # noqa: E402
from unicorn.m68k_const import (  # noqa: E402
    UC_M68K_REG_A7,
    UC_M68K_REG_D0,
    UC_M68K_REG_PC,
)

import tools.plusdrive as pd  # noqa: E402

DEFAULT_SNAPSHOT = os.path.join("snapshots", "dt2-1.16", "running.snap")

# Functions (DT2 1.16 MAIN OS).
EMMC_READ = 0x4012DEDA
EMMC_WRITE = 0x4012E0C0
MUTEX_LOCK = 0x400015A0
MUTEX_UNLOCK = 0x400016D2
RMUTEX_LOCK = 0x40001608
RMUTEX_UNLOCK = 0x4000172A
FLEXBUS_PAGE = 0x400CD638
LOADER_EVENT = 0x40153622
MOUNT = 0x4015A450
ID_VALID = 0x4015B93E
HASH_FIND = 0x4015A94C
RESOLVE_REF = 0x4015B178
REF_KEY = 0x4015ABE6
REHASH = 0x4015AF0C
READ_SECTORS = 0x400EFC8E
COKI_CHECK = 0x400C0E90
DECODE_PROJECT = 0x400C0C2C
PROJECT_GET = 0x401988A2
SETTINGS_GET = 0x40041D18
SETTINGS_KIT_ARG = 0x4004AEEA
SLOT_LIST = 0x4004E598
PROJECT_KIT = 0xF4  # FUN_40041d24: the active kit object is Project + 0xf4
KIT_DATA_VFUNC = 0x30  # kit vtable slot: (kit, kit_arg) -> 16 track records
TRACK_STRIDE = 0x450
TRACK_MACHINE = 0xD6  # machine type byte (Sound + 0xa2, the DSP row's byte 0)
TRACK_SLOT = 0x80  # int16 sample slot (Sound + 0x4c, mirror index 28)
LOAD_SAMPLE = 0x40154540
PATH_OF_ID = 0x401575F6
LOOKUP_NAME = 0x401572E0
NAME_HASH = 0x40155F96
DIR_CURSOR = 0x4015705C
READDIR = 0x40157072

# Data.
MOUNTED = 0x44F2BD68
HASH_INDEX_COUNT = 0x4067D8C0
HASH_INDEX = 0x4067D8C4  # {hash, id} pairs
PROJECT_BUF = 0x4099D588
PROJECT_REINIT = 0x4099D584
LOADER_KEYS = 0x405B1368  # slot * 16
LOADER_LEN = 0x405BA768
LOADER_RATE = 0x405BB768
LOADER_STEREO = 0x405BA368
LOADER_NAME_PTR = 0x405BC768

SCRATCH = 0x10200000  # host-owned scratch page for arguments
STACK_TOP = 0x10100000 - 0x100

_MASK = 0xFFFFFFFF


def _s32(v):
    return v - (1 << 32) if v & 0x80000000 else v


class Card:
    """The image file plus a write overlay, by 512-byte sector."""

    def __init__(self, path):
        self._f = open(path, "rb")  # noqa: SIM115 -- kept open for the object's life
        self.overlay = {}
        self.reads = []
        self.writes = []

    def read(self, sector, length):
        out = bytearray()
        n = (length + pd.SECTOR - 1) // pd.SECTOR
        for s in range(sector, sector + n):
            blk = self.overlay.get(s)
            if blk is None:
                self._f.seek(s * pd.SECTOR)
                blk = self._f.read(pd.SECTOR).ljust(pd.SECTOR, b"\0")
            out += blk
        return bytes(out[:length])

    def write(self, sector, data):
        data = bytes(data).ljust(-(-len(data) // pd.SECTOR) * pd.SECTOR, b"\0")
        for i in range(len(data) // pd.SECTOR):
            self.overlay[sector + i] = data[i * pd.SECTOR : (i + 1) * pd.SECTOR]

    def close(self):
        self._f.close()


class Firmware:
    """A restored snapshot with the HLEs above installed."""

    def __init__(self, image_path, snapshot=DEFAULT_SNAPSHOT, main_os=None):
        from emu import config
        from emu import snapshot as snap

        main_os = main_os or config.main_image()
        with open(main_os, "rb") as fh:
            image = fh.read()
        m, _, _ = snap.restore(snapshot)
        # The snapshot holds the code and its initialised data as they are
        # at run time (reloading the image would reset .data); the image is
        # only scanned for the ColdFire opcodes Unicorn lacks.
        if bytes(m.uc.mem_read(pd.MAIN_OS_BASE, 0x1000)) != image[:0x1000]:
            raise ValueError("snapshot %s does not hold this MAIN OS" % snapshot)
        m.install_isa_patches_scoped(image, pd.MAIN_OS_BASE)
        m.ensure(SCRATCH)
        m.ensure(STACK_TOP)
        self.m = m
        self.uc = m.uc
        self.card = Card(image_path)
        self.pages = []  # (page word, 4 KiB bytes, hdr flag)
        self.events = []  # (event, slot)
        self.traps = []
        self._scratch = SCRATCH
        self._hle(EMMC_READ, self._emmc_read)
        self._hle(EMMC_WRITE, self._emmc_write)
        self._hle(MUTEX_LOCK, lambda a: 1)
        self._hle(MUTEX_UNLOCK, lambda a: 0)
        self._hle(RMUTEX_LOCK, lambda a: 1)
        self._hle(RMUTEX_UNLOCK, lambda a: 0)
        self._hle(FLEXBUS_PAGE, self._flexbus_page)
        self._hle(LOADER_EVENT, self._loader_event)
        self.uc.hook_add(UC_HOOK_INTR, self._on_intr)

    # -- memory helpers ----------------------------------------------------
    def u32(self, addr):
        return struct.unpack(">I", self.uc.mem_read(addr, 4))[0]

    def s16(self, addr):
        return struct.unpack(">h", self.uc.mem_read(addr, 2))[0]

    def read(self, addr, n):
        return bytes(self.uc.mem_read(addr, n))

    def write(self, addr, data):
        self.uc.mem_write(addr, bytes(data))

    def alloc(self, data_or_size):
        """-> address of a fresh scratch buffer (zeroed, or holding data)."""
        data = bytes(data_or_size)  # bytes(n) is n zero bytes
        addr = self._scratch
        self._scratch += (len(data) + 15) & ~15
        self.write(addr, data)
        return addr

    # -- HLE ---------------------------------------------------------------
    def _hle(self, addr, fn):
        uc = self.uc

        def hook(uc_, a, size, data):
            sp = uc.reg_read(UC_M68K_REG_A7)
            ret = self.u32(sp)

            def arg(i):
                return self.u32(sp + 4 + 4 * i)

            d0 = fn(arg)
            uc.reg_write(UC_M68K_REG_D0, (d0 or 0) & _MASK)
            uc.reg_write(UC_M68K_REG_A7, sp + 4)
            uc.reg_write(UC_M68K_REG_PC, ret)

        uc.hook_add(UC_HOOK_CODE, hook, begin=addr, end=addr)

    def _emmc_read(self, arg):
        sector, length, dest = arg(0), _s32(arg(1)), arg(2)
        self.card.reads.append((sector, length))
        self.write(dest, self.card.read(sector, length))
        return 0

    def _emmc_write(self, arg):
        sector, length, src = arg(0), _s32(arg(1)), arg(2)
        self.card.writes.append((sector, length))
        self.card.write(sector, self.read(src, length))
        return 0

    def _flexbus_page(self, arg):
        self.pages.append((arg(0), self.read(arg(1), 0x1000), arg(2)))
        return 0

    def _loader_event(self, arg):
        self.events.append((arg(0), arg(1)))
        return 0

    def _on_intr(self, uc, vec, data):
        self.traps.append((vec, uc.reg_read(UC_M68K_REG_PC)))
        uc.emu_stop()

    # -- calls -------------------------------------------------------------
    def call(self, func, args, limit=20_000_000):
        from emu import harness

        self.traps.clear()
        d0 = harness.call(self.m, func, args, stack_top=STACK_TOP, limit=limit)
        if self.traps:
            raise RuntimeError(
                "FUN_%08x blocked or trapped: %s"
                % (func, ", ".join("vec %#x at %#x" % t for t in self.traps))
            )
        pc = self.uc.reg_read(UC_M68K_REG_PC)
        if pc != 0xDEADBEE0:
            raise RuntimeError(
                "FUN_%08x did not return within %d instructions (pc %#x)"
                % (func, limit, pc)
            )
        return d0

    def close(self):
        self.card.close()


def image_file_ref(image_path, record_id):
    """-> the 16-byte sample reference {id, hash|1, size, seq} read back from
    the image's record for `record_id`."""
    with open(image_path, "rb") as f:
        f.seek(pd._record_offset(record_id))
        rec = f.read(pd.RECORD_SIZE)
    size, _, hash_word, seq = struct.unpack_from(">IIII", rec, 0x04)
    return struct.pack(">IIII", record_id, hash_word, size, seq)


def image_file_bytes(image_path, record_id):
    """-> the file's content, read through its record's inline extents."""
    with open(image_path, "rb") as f:
        img = pd._ReadOnlyImage(f)
        rec = pd._read_record(img, record_id)
        data = b"".join(
            pd._read_page(img, phys + i)
            for logical, length, phys in rec["extents"]
            if logical < 0x10000
            for i in range(length)
        )
    return data[: rec["size"]]


def sent_pcm_matches(load, native):
    """Rebuild sample memory from the pages FUN_40154540 sent (page word *
    0x1000, block based at 0x400) and compare each channel at the start the
    slot header gives with the file's PCM. -> list of bools, one per
    channel."""
    stereo, data_len, _ = pd.parse_native_header(native)
    mem = bytearray()
    for word, page, _ in load["pages"]:
        end = (word + 1) * 0x1000
        if len(mem) < end:
            mem += bytes(end - len(mem))
        mem[word * 0x1000 : end] = page
    header = load["slot_headers"][-1]
    pcm = native[pd.NATIVE_HEADER_SIZE : pd.NATIVE_HEADER_SIZE + data_len]
    if not stereo:
        start = header[1]
        return [bytes(mem[start : start + data_len]) == pcm]
    frames = [pcm[i : i + 4] for i in range(0, len(pcm), 4)]
    left = b"".join(f[:2] for f in frames)
    right = b"".join(f[2:] for f in frames)
    return [
        bytes(mem[header[1] : header[1] + len(left)]) == left,
        bytes(mem[header[2] : header[2] + len(right)]) == right,
    ]


def check_mount(fw, ref):
    record_id, hash_word = struct.unpack_from(">II", ref)
    d0 = fw.call(MOUNT, [0], limit=200_000_000)
    count = fw.u32(HASH_INDEX_COUNT)
    index = [
        struct.unpack_from(">II", fw.read(HASH_INDEX + 8 * i, 8)) for i in range(count)
    ]
    found = fw.call(HASH_FIND, [hash_word, 0])
    return {
        "mount_d0": d0,
        "mounted": fw.u32(MOUNTED),
        "id_valid": fw.call(ID_VALID, [record_id]),
        "hash_index_count": count,
        "hash_index_has_file": (hash_word, record_id) in index,
        "hash_find": _s32(found),
        "hash_find_id": (index[_s32(found)][1] if 0 <= _s32(found) < count else None),
    }


def check_directory(fw, record_id, name):
    """The root directory as the firmware walks it: readdir (0x10001),
    lookup by name (0x10000 and FUN_40155f96), and the path of an id
    (content page 0's ".." entry and 0x10002), which the loader uses for the
    slot's name."""
    listing = []
    cursor = fw.alloc(16)
    entry = fw.alloc(0x120)
    fw.call(DIR_CURSOR, [pd.ROOT_ID, cursor])
    for _ in range(16):
        if _s32(fw.call(READDIR, [cursor, entry])) < 1:
            break
        listing.append((fw.read(entry + 0x14, 0x100).split(b"\0")[0], fw.u32(entry)))
    name_addr = fw.alloc(name + b"\0")
    found = fw.alloc(0x120)
    lookup = fw.call(LOOKUP_NAME, [pd.ROOT_ID, name_addr, found])
    hashes = {}
    for n in (b".", b"..", name):
        addr = fw.alloc(n)
        hashes[n] = (fw.call(NAME_HASH, [addr, len(n)]), pd.name_hash(n))
    path_buf = fw.alloc(0x100)
    path_ptr = fw.call(PATH_OF_ID, [record_id, 0, path_buf])
    path = fw.read(path_ptr, 0x100).split(b"\0")[0] if path_ptr else None
    return {
        "listing": listing,
        "lookup": lookup,
        "lookup_id": fw.u32(found),
        "hashes": hashes,
        "path": path,
    }


def check_resolve(fw, ref):
    record_id = struct.unpack_from(">I", ref)[0]
    ref_addr = fw.alloc(ref)
    out_id = fw.alloc(4)
    d0 = fw.call(RESOLVE_REF, [ref_addr, out_id])
    key = fw.alloc(16)
    k0 = fw.call(REF_KEY, [record_id, key])
    return {
        "resolve_d0": _s32(d0),
        "resolve_id": fw.u32(out_id),
        "key_d0": _s32(k0),
        "key": fw.read(key, 16),
    }


def check_rehash(fw, ref):
    record_id, hash_word = struct.unpack_from(">II", ref)
    d0 = fw.call(REHASH, [record_id], limit=400_000_000)
    sector = pd.HASH_TABLE_SECTOR + (record_id >> 13) * (pd.PAGE // pd.SECTOR)
    page = fw.card.read(sector, pd.PAGE)
    written = struct.unpack_from(">I", page, (record_id & 0x1FFF) * 4)[0]
    return {"rehash_d0": d0, "firmware_hash": written, "image_hash": hash_word}


def check_project(fw):
    fw.call(READ_SECTORS, [pd.BOOT_CONFIG_SECTOR, pd.PROJECT_RECORD_SIZE, PROJECT_BUF])
    coki = fw.call(COKI_CHECK, [])
    flags_word = fw.u32(PROJECT_BUF + 0x14)
    fw.write(PROJECT_REINIT, b"\xff\xff\xff\xff")
    fw.call(DECODE_PROJECT, [0], limit=600_000_000)
    reinit = fw.u32(PROJECT_REINIT)
    version = fw.u32(PROJECT_BUF + pd.PROJECT_HEADER_SIZE + 4)
    project = fw.call(PROJECT_GET, [])
    settings = fw.call(SETTINGS_GET, [project])
    kit_arg = fw.call(SETTINGS_KIT_ARG, [settings])
    # FUN_4004e598(&{&vector, &kit_arg, settings}), as FUN_4004e8be calls it.
    vec_buf = fw.alloc(0x400 * 4)
    vec = fw.alloc(struct.pack(">III", vec_buf, vec_buf, vec_buf + 0x1000))
    karg = fw.alloc(struct.pack(">I", kit_arg))
    frame = fw.alloc(struct.pack(">III", vec, karg, settings))
    fw.call(SLOT_LIST, [frame], limit=50_000_000)
    begin, end = fw.u32(vec), fw.u32(vec + 4)
    slots = [fw.u32(a) for a in range(begin, end, 4)]
    # ProjectSettings data (vtable +0x28) + 2 + (slot + 0x1d) * 16.
    vtable = fw.u32(settings)
    data = fw.call(fw.u32(vtable + 0x28), [settings])
    refs = {s: fw.read(data + 2 + (s + 0x1D) * 16, 16) for s in slots[:16]}
    # The active kit's track records, as FUN_4004e598 reads them.
    kit = project + PROJECT_KIT
    kit_data = fw.call(fw.u32(fw.u32(kit) + KIT_DATA_VFUNC), [kit, kit_arg])
    tracks = [
        (
            fw.read(kit_data + t * TRACK_STRIDE + TRACK_MACHINE, 1)[0],
            fw.s16(kit_data + t * TRACK_STRIDE + TRACK_SLOT),
        )
        for t in range(16)
    ]
    return {
        "coki_ok": coki & 0xFF,
        "header_word_0x14": flags_word,
        "reinit": reinit,
        "container_version": version,
        "project": project,
        "settings": settings,
        "settings_data": data,
        "slots": slots,
        "refs": refs,
        "kit_data": kit_data,
        "tracks": tracks,
    }


def check_load(fw, ref, slot):
    fw.pages.clear()
    fw.events.clear()
    ref_addr = fw.alloc(ref)
    d0 = fw.call(LOAD_SAMPLE, [ref_addr, slot], limit=800_000_000)
    headers = [p for p in fw.pages if p[0] == 0xFFFFFFFF]
    data_pages = [p for p in fw.pages if p[0] != 0xFFFFFFFF]
    return {
        "load_d0": _s32(d0),
        "data_pages": len(data_pages),
        "first_page_word": data_pages[0][0] if data_pages else None,
        "last_page_word": data_pages[-1][0] if data_pages else None,
        "slot_headers": [struct.unpack_from(">IIIII", h[1]) for h in headers],
        "pages": data_pages,
        "events": list(fw.events),
        "key": fw.read(LOADER_KEYS + slot * 16, 16),
        "len": fw.u32(LOADER_LEN + slot * 4),
        "rate": fw.u32(LOADER_RATE + slot * 4),
        "stereo": fw.read(LOADER_STEREO + slot, 1)[0],
        # The loader copies the basename of FUN_401575f6's path for the id
        # to 0x405b6368 + slot * 16 and keeps a pointer per slot.
        "name": fw.read(fw.u32(LOADER_NAME_PTR + slot * 4), 16).split(b"\0")[0],
    }


def run_checks(
    image_path, record_id=pd.ROOT_ID + 1, snapshot=DEFAULT_SNAPSHOT, main_os=None
):
    """-> dict of every check's results (see the module docstring)."""
    ref = image_file_ref(image_path, record_id)
    name = next(e["name"] for e in pd.ls(image_path) if e["id"] == record_id)
    name = name.encode("ascii")
    fw = Firmware(image_path, snapshot, main_os)
    try:
        out = {"ref": ref}
        out["mount"] = check_mount(fw, ref)
        out["resolve"] = check_resolve(fw, ref)
        out["directory"] = check_directory(fw, record_id, name)
        out["project"] = check_project(fw)
        # Load the first slot "Load all samples" would load (the active
        # kit's first track), then a second slot with the same reference:
        # the loader must alias the block it already sent (header only).
        slots = [s for s in out["project"]["slots"] if s & 0x7F]
        out["load"] = check_load(fw, ref, slots[0])
        out["load"]["slot"] = slots[0]
        out["load"]["pcm_matches"] = sent_pcm_matches(
            out["load"], image_file_bytes(image_path, record_id)
        )
        out["alias"] = check_load(fw, ref, slots[1])
        out["alias"]["slot"] = slots[1]
        out["rehash"] = check_rehash(fw, ref)
        return out
    finally:
        fw.close()


def main(argv=None):
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("image")
    p.add_argument("--id", type=int, default=pd.ROOT_ID + 1, help="record id to check")
    p.add_argument("--snapshot", default=DEFAULT_SNAPSHOT)
    p.add_argument("--main-os", default=pd.DEFAULT_MAIN_OS)
    args = p.parse_args(argv)
    r = run_checks(args.image, args.id, args.snapshot, args.main_os)
    print("ref            %s" % r["ref"].hex())
    for k, v in r["mount"].items():
        print("mount   %-22s %s" % (k, hex(v) if isinstance(v, int) else v))
    for k, v in r["directory"].items():
        print("dir     %-22s %s" % (k, v))
    for k, v in r["resolve"].items():
        print("resolve %-22s %s" % (k, v.hex() if isinstance(v, bytes) else v))
    proj = r["project"]
    for k in (
        "coki_ok",
        "header_word_0x14",
        "reinit",
        "container_version",
        "project",
        "settings",
        "settings_data",
        "kit_data",
    ):
        print("project %-22s %#x" % (k, proj[k]))
    print("project slots (load order) %s" % proj["slots"][:24])
    for t, (machine, slot) in enumerate(proj["tracks"]):
        print("project track %-2d machine %d slot %d" % (t + 1, machine, slot))
    for s, ref in proj["refs"].items():
        print("project slot %-4d ref %s" % (s, ref.hex()))
    for name in ("load", "alias"):
        load = r[name]
        for k in ("slot", "load_d0", "data_pages", "first_page_word", "last_page_word"):
            print("%-7s %-22s %s" % (name, k, load[k]))
        for h in load["slot_headers"]:
            print("%-7s slot header %s" % (name, " ".join("%#x" % w for w in h)))
        print(
            "%-7s key %s len %d rate %d stereo %d name %r events %s"
            % (
                name,
                load["key"].hex(),
                load["len"],
                load["rate"],
                load["stereo"],
                load["name"],
                load["events"],
            )
        )
    print("load    pcm_matches (per channel)  %s" % r["load"]["pcm_matches"])
    for k, v in r["rehash"].items():
        print("rehash  %-22s %#x" % (k, v))
    return 0


if __name__ == "__main__":
    sys.exit(main())
