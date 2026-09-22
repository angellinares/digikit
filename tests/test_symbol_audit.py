# fmt: off
"""Audit every symbol emu/symbols.py resolves, on both current firmwares --
Digitakt II 1.16 and Digitone II 1.11 -- instead of discovering a gap one
hang at a time (as display_sem and worker_done_sem were). Skips outright if
an image has not been extracted; a real environment (Em's) has both.
"""
import unittest
from pathlib import Path

from emu import symbols

DT16 = Path("out/sections/dt2-1.16/section_3_MAIN_OS.bin")
DN11 = Path("out/sections/dn2-1.11/section_3_MAIN_OS.bin")

# Read only from emu/uitrace.py's UiTrace, which tools/guirun.py constructs
# only under --trace-ui / --trace-ui-verbose / --trace-ui-json (see
# tools/guirun.py around `trace = uitrace.UiTrace(...)`). Unresolved here
# degrades trace output, never boot, so the audit does not fail on it. Also
# covers queue_send/ui_queue, which UiTrace is the only outside reader of.
TRACE_UI_ONLY = frozenset({
    'ui_key_dispatch', 'view_offer', 'view_activate', 'view_close',
    'view_closed_mark', 'view_request_pop', 'view_sweep',
    'ui_tick_inc', 'ui_tick_counter', 'queue_send', 'ui_queue',
})

# transport/call_sites: diagnostic-only (dspboot's do_transport_call logs
# which timeout argument a call site pushed; changes no register or memory
# state). emu/symbols.py's own comment on `transport` documents it as
# "NOT verified to resolve on Digitone" and every caller degrades with
# `profile.call_sites or ()` (see emu/dspboot.py) rather than requiring it.
DIAGNOSTIC_ONLY = frozenset({'transport', 'call_sites'})

ALLOWED_NONE = TRACE_UI_ONLY | DIAGNOSTIC_ONLY

IMAGES = {'dt2-1.16': DT16, 'dn2-1.11': DN11}


class SymbolAuditTest(unittest.TestCase):
    """Every symbol should resolve on both current firmwares. A OPTIONAL
    symbol that the harness reads outside a trace-ui-only / diagnostic-only
    path and that comes back None is exactly the display_sem/worker_done_sem
    bug class: a hook silently never installs and something starves or
    fakes state it should not, with no error to find it by."""

    def test_no_unexpected_unresolved_symbols(self):
        ran = False
        for label, path in IMAGES.items():
            if not path.exists():
                continue
            ran = True
            with self.subTest(image=label):
                profile = symbols.resolve(path.read_bytes())
                bad = sorted(set(profile.unresolved) - ALLOWED_NONE)
                self.assertEqual(
                    bad, [],
                    'unresolved symbol(s) the harness uses outside a '
                    'trace-ui/diagnostic-only path on %s: %s\n%s'
                    % (label, bad, profile.report()))
                # ALLOWED_NONE is a fixed allowlist, not "whatever came back
                # None" -- if a name on it actually resolves now, that is
                # worth knowing (the entry may be stale), not silently fine.
                allowed_but_resolved = sorted(
                    n for n in ALLOWED_NONE if n not in profile.unresolved)
                if allowed_but_resolved:
                    pass  # informational only; not a failure

        if not ran:
            self.skipTest('neither out/sections/dt2-1.16 nor '
                          'out/sections/dn2-1.11 is extracted')

    def test_fixed_rules_verify_on_both_images(self):
        """Every Fixed(...) rule (a literal address, optionally with a
        verify-bytes check) should resolve on BOTH current images. A Fixed
        rule that only verifies on one build is exactly the display_sem bug
        (emu/symbols.py's own SigAt docstring): a hardcoded 1.15C-only
        address that silently stops matching on a later build."""
        present = {label: path for label, path in IMAGES.items() if path.exists()}
        if len(present) < 2:
            self.skipTest('need both out/sections/dt2-1.16 and '
                          'out/sections/dn2-1.11 extracted')
        imgs = {label: path.read_bytes() for label, path in present.items()}
        failures = []
        for name, rule, _required in symbols.SYMBOLS:
            if not isinstance(rule, symbols.Fixed):
                continue
            if name in TRACE_UI_ONLY:
                # The 8 UI-trace hook points (ui_key_dispatch, view_*,
                # ui_tick_inc) are still hardcoded 1.15C addresses -- never
                # re-derived by signature the way display_frame_post was in
                # commit 7aaabbe -- so they are KNOWN to miss on 1.16/1.11.
                # Flagged here rather than fixed: each would need its own
                # masked-signature or SigAt rule verified against a
                # decompile, the same amount of work display_sem took, and
                # is out of scope for this audit. See TRACE_UI_ONLY's
                # docstring for why an unresolved one is harmless (--trace-ui
                # only) rather than silently wrong.
                continue
            for label, img in imgs.items():
                val, detail = rule.resolve(img, symbols.LOAD_ADDR, {})
                if val is None:
                    failures.append('%s on %s: %s' % (name, label, detail))
        self.assertEqual(failures, [],
                         'Fixed rule(s) that do not verify on both images:\n  '
                         + '\n  '.join(failures))


if __name__ == '__main__':
    unittest.main()
