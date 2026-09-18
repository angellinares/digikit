"""Bounded 1.16 source-object -> SRAM row -> DSPI frame experiment.

This deliberately invokes FUN_4002d438(src, track) before vector 191.  It
proves that the selected source object's +0xa2 byte is copied into its 0x9a
SRAM row and reaches the transmitted frame; it does not establish that a UI
setter notification or cache invalidation reaches that refresh.
"""

import argparse
import hashlib
import json
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import framelink  # noqa: E402
from unicorn import (  # type: ignore[import-not-found]  # noqa: E402
    UC_HOOK_MEM_WRITE,
    UcError,
)
from unicorn.m68k_const import (  # type: ignore[import-not-found]  # noqa: E402
    UC_M68K_REG_A7,
    UC_M68K_REG_D0,
    UC_M68K_REG_PC,
)

from emu.harness import call  # noqa: E402

IMAGE_SHA256 = "57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d"
LIVE_BASE = 0x80004704
TRACK_STRIDE, TRACK_OFF, TYPE_OFF = 0x450, 0x34, 0xA2
ROW_BASE, ROW_STRIDE = 0x80003CD0, 0x9A
CACHE_BASE = 0x8000470C
FRAME_TYPE_OFF, FRAME_LENGTH = 0x94, 2050
ROW_REFRESH = 0x4002D438
MAX_INSTRUCTION_LIMIT = 100_000_000
BUILD_FLAGS = {
    "unblock": False,
    "softfloat": True,
    "bitmap": True,
    "dsp": True,
    "weakptr": False,
    "fast": False,
}


class BoundedRecords:
    """Small deterministic event sink; total includes dropped records."""

    def __init__(self, limit):
        if not 1 <= limit <= 128:
            raise ValueError("record limit must be 1..128")
        self.limit, self.total, self.records = limit, 0, []

    def add(self, record):
        self.total += 1
        if len(self.records) < self.limit:
            self.records.append(record)

    def result(self):
        return {
            "total": self.total,
            "emitted": len(self.records),
            "truncated": self.total > len(self.records),
            "records": self.records,
        }


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for part in iter(lambda: f.read(1024 * 1024), b""):
            h.update(part)
    return h.hexdigest()


def rdlong(uc, addr):
    return struct.unpack(">I", uc.mem_read(addr, 4))[0]


def rdbyte(uc, addr):
    return uc.mem_read(addr, 1)[0]


def frame_type(frame, track):
    off = FRAME_TYPE_OFF + 2 * track
    return (
        struct.unpack(">H", frame[off : off + 2])[0] if len(frame) >= off + 2 else None
    )


def frame_meta(frame, track):
    return {
        "length": len(frame),
        "sha256": hashlib.sha256(frame).hexdigest(),
        "type_word": frame_type(frame, track),
    }


def frame_diff(before, after):
    """Return changed bytes and coalesced ranges without retaining frame bytes."""
    changes = []
    n = max(len(before), len(after))
    for offset in range(n):
        old = before[offset : offset + 1]
        new = after[offset : offset + 1]
        if old != new:
            changes.append({"offset": offset, "before": old.hex(), "after": new.hex()})
    ranges, start, previous = [], -1, -2
    for change in changes:
        offset = change["offset"]
        if start < 0:
            start = offset
        elif offset != previous + 1:
            ranges.append(
                {
                    "start": start,
                    "end": previous + 1,
                    "before": before[start : previous + 1].hex(),
                    "after": after[start : previous + 1].hex(),
                }
            )
            start = offset
        previous = offset
    if start >= 0:
        ranges.append(
            {
                "start": start,
                "end": previous + 1,
                "before": before[start : previous + 1].hex(),
                "after": after[start : previous + 1].hex(),
            }
        )
    return {"bytes": changes, "ranges": ranges}


def frame_diffs(left, right):
    return [
        dict({"pass": index}, **frame_diff(a, b))
        for index, (a, b) in enumerate(zip(left, right))
    ]


def restore(snapshot, syx):
    from emu.longrun import build

    return build(
        snapshot,
        syx=syx,
        unblock=False,
        softfloat=True,
        bitmap=True,
        dsp=True,
        weakptr=False,
    )


def checkpoint_manifest(ev, image_sha):
    """Return the restored build contract, rejecting a mismatched --image."""
    manifest = dict(ev["checkpoint_manifest"])
    if manifest.get("main_sha256") != image_sha:
        raise SystemExit(
            "--image SHA does not match the MAIN image restored through DT2_SECTIONS"
        )
    return manifest


def stack_view(uc, sp, count):
    try:
        data = uc.mem_read(sp, count * 4)
        return {
            "longwords": [
                {"offset": i * 4, "value": struct.unpack_from(">I", data, i * 4)[0]}
                for i in range(count)
            ]
        }
    except Exception as exc:
        return {"error": "%s: %s" % (type(exc).__name__, exc)}


def install_observers(uc, at, row, record_limit, stack_longs):
    refresh, writes = BoundedRecords(record_limit), BoundedRecords(record_limit)

    def on_refresh(uc, addr, size, data):
        sp = uc.reg_read(UC_M68K_REG_A7)
        record = {"pc": addr, "sp": sp}
        try:
            ret, src, track = struct.unpack(">III", uc.mem_read(sp, 12))
            record.update({"return": ret, "args": [src, track]})
        except Exception as exc:
            record.update({"error": "%s: %s" % (type(exc).__name__, exc), "args": None})
        record.update(stack_view(uc, sp, stack_longs))
        refresh.add(record)

    def on_write(uc, kind, addr, size, value, data):
        writes.add(
            {
                "pc": uc.reg_read(UC_M68K_REG_PC),
                "sp": uc.reg_read(UC_M68K_REG_A7),
                "address": addr,
                "size": size,
                "value": value,
            }
        )

    at(ROW_REFRESH, on_refresh)
    uc.hook_add(UC_HOOK_MEM_WRITE, on_write, begin=row, end=row + ROW_STRIDE - 1)
    return refresh, writes


def drive(m, at, profile, track, passes, limit):
    """Raise vector 191 repeatedly and retain the last DSPI frame per pass."""
    uc, captured = m.uc, []

    def driver(uc, addr, size, data):
        sp = uc.reg_read(UC_M68K_REG_A7)
        ret, tx_len, tx, rx_len, rx = struct.unpack(">IIIII", uc.mem_read(sp, 20))
        captured.append(bytes(uc.mem_read(tx, tx_len)) if tx and tx_len else b"")
        uc.reg_write(UC_M68K_REG_D0, 0)
        uc.reg_write(UC_M68K_REG_A7, sp + 4)
        uc.reg_write(UC_M68K_REG_PC, ret)

    at(profile["driver"], driver)
    resume, frames, stops = uc.reg_read(UC_M68K_REG_PC), [], []
    for _ in range(passes):
        before = len(captured)
        uc.mem_write(profile["counter"], bytes(4))
        uc.reg_write(UC_M68K_REG_PC, resume)
        m.halt_vec = None
        try:
            if not m.raise_vector(profile["vector"]):
                raise RuntimeError("vector %d has no handler" % profile["vector"])
            uc.emu_start(uc.reg_read(UC_M68K_REG_PC), resume, count=limit)
            stop = "returned" if uc.reg_read(UC_M68K_REG_PC) == resume else "limit"
            if m.halt_vec is not None:
                stop = "unhandled vector %d" % m.halt_vec
        except (UcError, RuntimeError) as exc:
            stop = "%s: %s" % (type(exc).__name__, exc)
        frames.append(captured[-1] if len(captured) > before else b"")
        stops.append(stop)
    return frames, stops


def state(uc, obj, row, slot):
    return {
        "obj_type": rdbyte(uc, obj + TYPE_OFF),
        "row_type": rdbyte(uc, row),
        "cache": rdlong(uc, slot),
    }


def condition(args, profile, image_sha, name, poke, refresh, instrumented):
    m = None
    try:
        m, ev, st, pc, inq, at = restore(args.snapshot, args.syx)
        checkpoint_manifest(ev, image_sha)
        uc = m.uc
        live = rdlong(uc, LIVE_BASE)
        if live == 0:
            raise SystemExit(
                "_DAT_80004704 is zero: snapshot has no live track objects"
            )
        obj = live + args.track * TRACK_STRIDE + TRACK_OFF
        row, slot = ROW_BASE + args.track * ROW_STRIDE, CACHE_BASE + args.track * 4
        for address in (obj, row, slot):
            m.ensure(address)
        before = state(uc, obj, row, slot)
        row_before = bytes(uc.mem_read(row, ROW_STRIDE))
        observers = None
        if instrumented:
            observers = install_observers(
                uc, at, row, args.record_limit, args.stack_longs
            )
        if poke:
            uc.mem_write(obj + TYPE_OFF, bytes([args.new_type]))
        poked = state(uc, obj, row, slot)
        direct_return = None
        if refresh:
            direct_return = call(
                m, ROW_REFRESH, [obj, args.track], limit=args.limit
            )
        post_refresh = state(uc, obj, row, slot)
        if args.open_gate:
            uc.mem_write(profile["gate"], bytes(4))
        frames, stops = drive(m, at, profile, args.track, args.passes, args.limit)
        row_after = bytes(uc.mem_read(row, ROW_STRIDE))
        after = state(uc, obj, row, slot)
        result = {
            "condition": name,
            "variant": "instrumented" if instrumented else "clean",
            "track": args.track,
            "source": obj,
            "row": row,
            "cache_slot": slot,
            "before": before,
            "poked": poked,
            "post_refresh": post_refresh,
            "after": after,
            "direct_call": {"invoked": refresh, "d0": direct_return},
            "row_sha256": {
                "before": hashlib.sha256(row_before).hexdigest(),
                "after": hashlib.sha256(row_after).hexdigest(),
            },
            "frames": [frame_meta(frame, args.track) for frame in frames],
            "stops": stops,
            "_frame_bytes": frames,
            "_row_bytes": row_after,
        }
        if observers is None:
            result["refresh_records"] = BoundedRecords(args.record_limit).result()
            result["row_writes"] = BoundedRecords(args.record_limit).result()
        else:
            result["refresh_records"], result["row_writes"] = (
                observers[0].result(),
                observers[1].result(),
            )
        return result
    finally:
        if m is not None:
            m.close()


def equivalence(clean, watched):
    values = {
        "before": (clean["before"], watched["before"]),
        "poked": (clean["poked"], watched["poked"]),
        "post_refresh": (clean["post_refresh"], watched["post_refresh"]),
        "after": (clean["after"], watched["after"]),
        "direct_call": (clean["direct_call"], watched["direct_call"]),
        "row_sha256.before": (
            clean["row_sha256"]["before"],
            watched["row_sha256"]["before"],
        ),
        "row_sha256.after": (
            clean["row_sha256"]["after"],
            watched["row_sha256"]["after"],
        ),
        "frames": (clean["frames"], watched["frames"]),
        "stops": (clean["stops"], watched["stops"]),
    }
    mismatches = [
        {"field": name, "clean": pair[0], "instrumented": pair[1]}
        for name, pair in values.items()
        if pair[0] != pair[1]
    ]
    return {"equivalent": not mismatches, "mismatches": mismatches}


def accept(results, original, new_type, passes, track=0):
    """Structured gates for the four fresh-restore conditions."""
    reasons = []
    names = {"baseline", "source_only", "unchanged_refresh", "changed_refresh"}
    expected_pairs = {(name, variant) for name in names for variant in ("clean", "instrumented")}
    pairs = [(r.get("condition"), r.get("variant")) for r in results]
    if len(results) != 8 or len(set(pairs)) != 8 or set(pairs) != expected_pairs:
        reasons.append(
            {
                "gate": "condition_matrix",
                "detail": {"count": len(results), "pairs": pairs},
            }
        )
        return False, reasons, {}
    clean = {r["condition"]: r for r in results if r["variant"] == "clean"}
    watched = {r["condition"]: r for r in results if r["variant"] == "instrumented"}
    eq = {name: equivalence(clean[name], watched[name]) for name in sorted(names)}
    for name, value in eq.items():
        if not value["equivalent"]:
            reasons.append(
                {
                    "gate": "clean_instrumented_equivalence",
                    "condition": name,
                    "detail": value["mismatches"],
                }
            )
    for result in results:
        if any(stop != "returned" for stop in result["stops"]):
            reasons.append(
                {
                    "gate": "handler_stops",
                    "condition": result["condition"],
                    "detail": result["stops"],
                }
            )
        if len(result["frames"]) != passes or any(
            f["length"] != FRAME_LENGTH for f in result["frames"]
        ):
            reasons.append(
                {
                    "gate": "frame_length",
                    "condition": result["condition"],
                    "detail": result["frames"],
                }
            )
    anchor = clean["baseline"]
    expected_address = {
        "track": track,
        "source": anchor["source"],
        "row": ROW_BASE + track * ROW_STRIDE,
        "cache_slot": CACHE_BASE + track * 4,
    }
    if not expected_address["source"] or any(
        any(result.get(field) != value for field, value in expected_address.items())
        for result in results
    ):
        reasons.append(
            {"gate": "condition_identity", "detail": expected_address}
        )
    if any(result["before"] != anchor["before"] for result in results):
        reasons.append(
            {"gate": "fresh_state_identity", "detail": "before states differ"}
        )
    for name in ("baseline", "source_only"):
        r = watched[name]
        if r["refresh_records"]["total"] or r["row_writes"]["total"]:
            reasons.append(
                {
                    "gate": "no_refresh_without_direct_call",
                    "condition": name,
                    "detail": {
                        "refresh": r["refresh_records"]["total"],
                        "writes": r["row_writes"]["total"],
                    },
                }
            )
    for name in ("unchanged_refresh", "changed_refresh"):
        r = watched[name]
        records = r["refresh_records"]["records"]
        if (
            r["refresh_records"]["total"] != 1
            or not records
            or records[0].get("args") != [r["source"], track]
        ):
            reasons.append(
                {
                    "gate": "direct_refresh_hit",
                    "condition": name,
                    "detail": r["refresh_records"],
                }
            )
        if r["row_writes"]["total"] < 1:
            reasons.append(
                {
                    "gate": "direct_refresh_row_write",
                    "condition": name,
                    "detail": r["row_writes"],
                }
            )
    if clean["baseline"]["after"]["obj_type"] != original:
        reasons.append(
            {"gate": "baseline_source", "detail": clean["baseline"]["after"]}
        )
    if (
        clean["source_only"]["poked"]["obj_type"] != new_type
        or clean["source_only"]["post_refresh"]["obj_type"] != new_type
        or clean["source_only"]["after"]["obj_type"] != new_type
        or clean["source_only"]["after"]["row_type"] != original
    ):
        reasons.append(
            {"gate": "source_only_isolated", "detail": clean["source_only"]["after"]}
        )
    if clean["unchanged_refresh"]["after"]["row_type"] != original:
        reasons.append(
            {
                "gate": "unchanged_refresh_row",
                "detail": clean["unchanged_refresh"]["after"],
            }
        )
    if clean["unchanged_refresh"]["post_refresh"]["row_type"] != original:
        reasons.append(
            {
                "gate": "unchanged_refresh_post_row",
                "detail": clean["unchanged_refresh"]["post_refresh"],
            }
        )
    if [f["type_word"] for f in clean["unchanged_refresh"]["frames"]] != [
        original
    ] * passes:
        reasons.append(
            {
                "gate": "unchanged_refresh_frame_type",
                "detail": clean["unchanged_refresh"]["frames"],
            }
        )
    if clean["changed_refresh"]["after"]["row_type"] != new_type:
        reasons.append(
            {"gate": "changed_refresh_row", "detail": clean["changed_refresh"]["after"]}
        )
    if clean["changed_refresh"]["post_refresh"]["row_type"] != new_type:
        reasons.append(
            {
                "gate": "changed_refresh_post_row",
                "detail": clean["changed_refresh"]["post_refresh"],
            }
        )
    changed = clean["changed_refresh"]
    if any(
        changed[stage]["obj_type"] != new_type
        for stage in ("poked", "post_refresh", "after")
    ):
        reasons.append(
            {"gate": "changed_refresh_source", "detail": changed}
        )
    for name in ("unchanged_refresh", "changed_refresh"):
        if clean[name]["after"]["cache"] != clean[name]["source"]:
            reasons.append(
                {
                    "gate": "refresh_cache",
                    "condition": name,
                    "detail": clean[name]["after"],
                }
            )
    if [f["type_word"] for f in clean["source_only"]["frames"]] != [original] * passes:
        reasons.append(
            {"gate": "source_only_frame", "detail": clean["source_only"]["frames"]}
        )
    if clean["source_only"]["frames"] != clean["baseline"]["frames"]:
        reasons.append(
            {"gate": "source_only_frames", "detail": "does not match baseline"}
        )
    if [f["type_word"] for f in clean["baseline"]["frames"]] != [original] * passes:
        reasons.append(
            {"gate": "baseline_frame", "detail": clean["baseline"]["frames"]}
        )
    changed_words = [f["type_word"] for f in clean["changed_refresh"]["frames"]]
    if (
        len(changed_words) < 3
        or changed_words[0] != original
        or changed_words[1:3] != [new_type, new_type]
    ):
        reasons.append({"gate": "changed_refresh_frame_delay", "detail": changed_words})
    return not reasons, reasons, eq


def validate_args(args, image_sha):
    if image_sha != IMAGE_SHA256:
        raise SystemExit(
            "machinecommit only supports Digitakt II 1.16 MAIN SHA %s" % IMAGE_SHA256
        )
    if not 0 <= args.track <= 15:
        raise SystemExit("--track must be 0..15")
    if not 0 <= args.new_type <= 6:
        raise SystemExit("--type must be 0..6")
    if args.passes < 3:
        raise SystemExit("--passes must be at least 3 to observe the delayed frame")
    if not 1 <= args.limit <= MAX_INSTRUCTION_LIMIT:
        raise SystemExit(
            "--limit must be 1..%d instructions" % MAX_INSTRUCTION_LIMIT
        )
    if not 1 <= args.record_limit <= 128 or not 1 <= args.stack_longs <= 32:
        raise SystemExit(
            "--record-limit must be 1..128 and --stack-longs must be 1..32"
        )


def main():
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("snapshot")
    p.add_argument("--syx", required=False, default=os.environ.get("DT2_SYX"))
    p.add_argument(
        "--image", help="1.16 MAIN OS image (default: configured sections image)"
    )
    p.add_argument("--track", type=int, default=0)
    p.add_argument("--type", type=int, default=5, dest="new_type")
    p.add_argument("--passes", type=int, default=3)
    p.add_argument("--limit", type=int, default=5_000_000)
    p.add_argument("--record-limit", type=int, default=32)
    p.add_argument("--stack-longs", type=int, default=8)
    p.add_argument("--open-gate", action="store_true", default=True)
    p.add_argument("--no-open-gate", action="store_false", dest="open_gate")
    p.add_argument("--json")
    args = p.parse_args()
    if not args.syx:
        raise SystemExit("--syx or $DT2_SYX is required")
    image = args.image
    if image is None:
        from emu import config

        image = config.main_image()
    image_sha, profile = framelink.profile_for(image)
    validate_args(args, image_sha)
    snapshot_sha, syx_sha = sha256_file(args.snapshot), sha256_file(args.syx)
    # Validate new type from an immutable one-off restore before generating variants.
    probe = None
    try:
        probe, ev, st, pc, inq, at = restore(args.snapshot, args.syx)
        restored_manifest = checkpoint_manifest(ev, image_sha)
        live = rdlong(probe.uc, LIVE_BASE)
        if not live:
            raise SystemExit(
                "_DAT_80004704 is zero: snapshot has no live track objects"
            )
        original = rdbyte(
            probe.uc, live + args.track * TRACK_STRIDE + TRACK_OFF + TYPE_OFF
        )
    finally:
        if probe is not None:
            probe.close()
    if original == args.new_type:
        raise SystemExit("--type must differ from snapshot source type (%d)" % original)
    spec = [
        ("baseline", False, False),
        ("source_only", True, False),
        ("unchanged_refresh", False, True),
        ("changed_refresh", True, True),
    ]
    results = [
        condition(args, profile, image_sha, name, poke, refresh, instrumented)
        for instrumented in (False, True)
        for name, poke, refresh in spec
    ]
    accepted, reasons, equivalent = accept(
        results, original, args.new_type, args.passes, args.track
    )
    clean = {r["condition"]: r for r in results if r["variant"] == "clean"}
    diffs = {
        "changed_refresh_vs_unchanged_refresh": frame_diffs(
            clean["unchanged_refresh"]["_frame_bytes"],
            clean["changed_refresh"]["_frame_bytes"],
        ),
        "changed_refresh_vs_source_only": frame_diffs(
            clean["source_only"]["_frame_bytes"],
            clean["changed_refresh"]["_frame_bytes"],
        ),
    }
    row_diff = frame_diff(
        clean["unchanged_refresh"]["_row_bytes"],
        clean["changed_refresh"]["_row_bytes"],
    )
    for result in results:
        del result["_frame_bytes"]
        del result["_row_bytes"]
    output = {
        "provenance": {
            "snapshot": os.path.abspath(args.snapshot),
            "snapshot_sha256": snapshot_sha,
            "syx": os.path.abspath(args.syx),
            "syx_sha256": syx_sha,
            "image": os.path.abspath(image),
            "image_sha256": image_sha,
            "profile": profile["name"],
            "build_flags": BUILD_FLAGS,
            "checkpoint_manifest": restored_manifest,
            "passes": args.passes,
            "instruction_limit": args.limit,
            "record_limit": args.record_limit,
            "stack_longs": args.stack_longs,
            "track": args.track,
            "open_gate": args.open_gate,
        },
        "static_contract": {
            "refresh": {"pc": ROW_REFRESH, "signature": "FUN_4002d438(src, track)"},
            "source_type_offset": TYPE_OFF,
            "row": {"base": ROW_BASE, "stride": ROW_STRIDE},
            "cache_base": CACHE_BASE,
            "frame_type_offset": FRAME_TYPE_OFF,
            "scope": "Direct refresh does not establish setter notification or cache invalidation.",
        },
        "original_type": original,
        "new_type": args.new_type,
        "results": results,
        "frame_diffs": diffs,
        "row_diff_changed_vs_unchanged_refresh": row_diff,
        "equivalence": equivalent,
        "acceptance": {"accepted": accepted, "reasons": reasons},
    }
    if args.json:
        os.makedirs(os.path.dirname(os.path.abspath(args.json)), exist_ok=True)
        with open(args.json, "w") as f:
            json.dump(output, f, indent=2, sort_keys=True)
    print("A2 direct source->row->frame: %s" % ("ACCEPTED" if accepted else "REJECTED"))
    if not accepted:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
