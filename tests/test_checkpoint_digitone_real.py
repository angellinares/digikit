"""Opt-in stateful Digitone checkpoint regression; no proprietary inputs in-tree."""

import hashlib
import os
import tempfile
import unittest
from unittest.mock import patch

_ENV = ("DT2_DIGITONE_MAIN", "DT2_DIGITONE_SNAPSHOT", "DT2_DIGITONE_SYX")


@unittest.skipUnless(
    os.environ.get("DT2_CHECKPOINT_REAL") == "1"
    and all(os.environ.get(name) for name in _ENV),
    "set DT2_CHECKPOINT_REAL=1 and provide Digitone MAIN, snapshot, and syx",
)
class DigitoneCheckpointTest(unittest.TestCase):
    def test_stateful_checkpoint_preserves_real_cadence(self):
        from emu import checkpoint, config, longrun, snapshot, symbols
        from emu.dtim import Dtims, Timers
        from emu.pit import Pits, intro_running

        lead = int(os.environ.get("DT2_CHECKPOINT_LEAD", "120000000"))
        suffix = int(os.environ.get("DT2_CHECKPOINT_SUFFIX", "20000000"))
        syx = os.environ["DT2_DIGITONE_SYX"]
        env = {"DT2_MAIN_IMG": os.environ["DT2_DIGITONE_MAIN"], "DT2_SYX": syx}

        def build(path, **kwargs):
            return longrun.build(
                path,
                syx=syx,
                unblock=True,
                softfloat=True,
                bitmap=True,
                dsp=True,
                slc=True,
                **kwargs,
            )

        def guest_digest(machine):
            digest = hashlib.sha256()
            for base in sorted(machine.mapped):
                digest.update(base.to_bytes(4, "big"))
                digest.update(machine.uc.mem_read(base, snapshot.PAGE))
            return digest.hexdigest()

        def registers(machine):
            return tuple(machine.uc.reg_read(reg) for _name, reg in snapshot.REGS)

        def suffix_baseline(events):
            return {
                "tasks": len(events["tasks"]),
                "prints": len(events["prints"]),
                "switch_seq": len(events["switch_seq"]),
                "switch_last": events["switch_seq"][-1]
                if events["switch_seq"]
                else None,
                "uart_out": len(events["uart_out"]),
                "setpixel": longrun.setpixel_count(events),
                "pxcopy": events["pxcopy"],
                "satisfied": events["satisfied"],
            }

        def suffix_events(events, baseline):
            switches = [baseline["switch_last"]]
            switches.extend(events["switch_seq"][baseline["switch_seq"] :])
            normalized = []
            for tcb in switches:
                if tcb is not None and (not normalized or normalized[-1] != tcb):
                    normalized.append(tcb)
            return {
                "tasks": events["tasks"][baseline["tasks"] :],
                "prints": events["prints"][baseline["prints"] :],
                "switch_seq": normalized[1:],
                "uart_out": bytes(events["uart_out"][baseline["uart_out"] :]),
                "setpixel": longrun.setpixel_count(events) - baseline["setpixel"],
                "pxcopy": events["pxcopy"] - baseline["pxcopy"],
                "satisfied": events["satisfied"] - baseline["satisfied"],
            }

        def signature(machine, events, timers, baseline, done, stop):
            tx = events["edma_tx"]
            return {
                "done": done,
                "stop": stop,
                "registers": registers(machine),
                "guest": guest_digest(machine),
                "events": suffix_events(events, baseline),
                "timers": timers.checkpoint_state(),
                "edma": tx.checkpoint_state(),
                "tcd": bytes(machine.uc.mem_read(tx.tcd, 0x20)),
                "uart_in": tuple(events["checkpoint_components"]["uart_in"]),
            }

        with (
            patch.dict(os.environ, env, clear=False),
            tempfile.TemporaryDirectory() as directory,
        ):
            source = os.environ["DT2_DIGITONE_SNAPSHOT"]
            machine, events, state, pc, _inq, at = build(source)
            with open(config.main_image(), "rb") as image:
                profile = symbols.resolve(image.read())
            hold = intro_running(machine, profile.intro_pit3_isr)
            timers = Timers(
                Pits(machine, hold=hold), Dtims(machine, channels=(3,), hold=hold)
            )
            if timers.held:
                at(profile.intro_done, lambda _u, _a, _s, _d: timers.release())
            try:
                pc, lead_done, lead_stop = longrun.spin(machine, pc, lead, pits=timers)
                self.assertEqual(lead_stop, "limit")
                path = os.path.join(directory, "digitone-stateful.snap")
                checkpoint.save_longrun(
                    machine,
                    events,
                    timers,
                    path,
                    {"n": state["n"] + lead_done},
                )
                baseline = suffix_baseline(events)
                pc, direct_done, direct_stop = longrun.spin(
                    machine, pc, suffix, pits=timers
                )
                direct = signature(
                    machine, events, timers, baseline, direct_done, direct_stop
                )
            finally:
                machine.close()

            restored, restored_events, _state, restored_pc, _inq, _at = build(
                path, deferred_components=("timers",)
            )
            restored_timers = restored_events["restore_checkpoint_timers"]()
            restored_baseline = suffix_baseline(restored_events)
            restored_baseline["switch_last"] = baseline["switch_last"]
            try:
                restored_pc, resumed_done, resumed_stop = longrun.spin(
                    restored, restored_pc, suffix, pits=restored_timers
                )
                resumed = signature(
                    restored,
                    restored_events,
                    restored_timers,
                    restored_baseline,
                    resumed_done,
                    resumed_stop,
                )
            finally:
                restored.close()

            self.assertEqual(resumed, direct)


if __name__ == "__main__":
    unittest.main()
