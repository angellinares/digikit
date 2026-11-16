//! The state of each module of the boot selection (docs/plans/runtime-and-services.md section 8, item R11), defined once.
//!
//! ```text
//! SELECTED -> PLANNED or REFUSED(reason)      the boot plan (`plan`)
//! PLANNED  -> INIT                            the loader starts the module
//! INIT     -> ACTIVE                          loaded, no DSP part
//! INIT     -> DSP_LOADING -> ACTIVE           the ColdFire part is live, the DSP part follows
//! INIT or DSP_LOADING -> FAILED(reason)       the load or the DSP part failed
//! SELECTED -> SKIPPED                         safe mode loads nothing
//! ```
//!
//! The states and their numbers are `[load.state]` of `sdk/sdk.toml`. `BootStates` is a view of the two arrays of `DkrtBootInfo` (`states`, `detail`): a
//! transition that does not start from the state it needs does nothing and answers `false`, so a late or repeated call cannot move a module backwards. The
//! reason of REFUSED and FAILED is the module's load status (`DkrtBootInfo.results`); REFUSED also keeps the planner's detail words (`Reason::detail`).
//! "DSP ready" as a call to the module is not part of this (R12).
//!
//! `loads_v2` writes the `LOADS` reply (`[protocol.commands]`); the layout is pure so a host test pins it.
use dkrt_sdk::*;

/// Bytes of a module's file name in `LOADS` (the part of its path after the last `/`, NUL padded; at most `NAME_LEN - 1` characters).
pub const NAME_LEN: usize = 28;
/// Version byte of the `LOADS` reply this crate writes.
pub const LOADS_VERSION: u8 = 2;
/// Bytes of one module in the version 2 reply: `i32 result`, `u8 state`, 3 zero bytes, `u32 detail[2]`, `needs[NEEDS_LEN]`, `name[NAME_LEN]`.
pub const LOADS_ENTRY: usize = 4 + 4 + 8 + NEEDS_LEN + NAME_LEN;
/// Bytes of the reply for `n` modules (the version, the count, the entries).
pub const fn loads_len(n: usize) -> usize {
    2 + LOADS_ENTRY * n
}

/// The state a finished load attempt leaves a module in, from its load status alone: `OK` is ACTIVE, `LOADING` is DSP_LOADING, `E_SAFE_MODE` is SKIPPED, any
/// other code is FAILED. REFUSED is not here: it is the planner's verdict (a code such as `E_CONTRACT` or `E_MISSING` is a refusal when the planner says it
/// and a failure when the loader does).
pub fn classify(code: i32) -> u8 {
    if code == OK {
        STATE_ACTIVE
    } else if code == LOADING {
        STATE_DSP_LOADING
    } else if code == E_SAFE_MODE {
        STATE_SKIPPED
    } else {
        STATE_FAILED
    }
}

/// A view of the `states` and `detail` arrays of `DkrtBootInfo`.
pub struct BootStates<'a> {
    pub states: &'a mut [u8; SEL_MAX],
    pub detail: &'a mut [[u32; 2]; SEL_MAX],
}

impl BootStates<'_> {
    /// Move module `k` from `from` to `to`; false (and no change) when it is not in `from`.
    fn step(&mut self, k: usize, from: u8, to: u8) -> bool {
        if k < SEL_MAX && self.states[k] == from {
            self.states[k] = to;
            true
        } else {
            false
        }
    }

    /// The start of a boot: every module SELECTED, no detail.
    pub fn begin(&mut self) {
        *self.states = [STATE_SELECTED; SEL_MAX];
        *self.detail = [[0; 2]; SEL_MAX];
    }

    /// The planner's verdict for module `k`: PLANNED, or REFUSED with the reason's detail words.
    pub fn planned(&mut self, k: usize, verdict: crate::plan::Verdict) -> bool {
        if let crate::plan::Verdict::Refused(why) = verdict {
            let moved = self.step(k, STATE_SELECTED, STATE_REFUSED);
            if moved {
                self.detail[k] = why.detail();
            }
            moved
        } else {
            self.step(k, STATE_SELECTED, STATE_PLANNED)
        }
    }

    /// The loader starts module `k`.
    pub fn init(&mut self, k: usize) -> bool {
        self.step(k, STATE_PLANNED, STATE_INIT)
    }

    /// The load of module `k` returned `code`: ACTIVE, DSP_LOADING or FAILED.
    pub fn finished(&mut self, k: usize, code: i32) -> bool {
        self.step(k, STATE_INIT, classify(code))
    }

    /// The DSP part of module `k` ended with `code`: ACTIVE or FAILED.
    pub fn dsp_done(&mut self, k: usize, code: i32) -> bool {
        let to = if code == OK {
            STATE_ACTIVE
        } else {
            STATE_FAILED
        };
        self.step(k, STATE_DSP_LOADING, to)
    }

    /// Safe mode: the first `count` modules are SKIPPED.
    pub fn skip_all(&mut self, count: usize) {
        let mut k = 0;
        while k < count && k < SEL_MAX {
            self.step(k, STATE_SELECTED, STATE_SKIPPED);
            k += 1;
        }
    }
}

/// What `loads_v2` reads: the boot's per-module records.
pub struct LoadsView<'a> {
    pub count: usize,
    pub results: &'a [i32; SEL_MAX],
    pub states: &'a [u8; SEL_MAX],
    pub detail: &'a [[u32; 2]; SEL_MAX],
    pub needs: &'a [[u8; NEEDS_LEN]; SEL_MAX],
    pub names: &'a [[u8; NAME_LEN]; SEL_MAX],
}

/// Write the version 2 `LOADS` reply body into `out` (at least `loads_len(SEL_MAX)` bytes): `u8 version`, `u8 n`, then per module in selection order the
/// `LOADS_ENTRY` bytes, every number big endian. Returns the bytes written.
pub fn loads_v2(v: &LoadsView, out: &mut [u8]) -> usize {
    let n = if v.count > SEL_MAX { SEL_MAX } else { v.count };
    out[0] = LOADS_VERSION;
    out[1] = n as u8;
    let mut at = 2;
    let mut k = 0;
    while k < n {
        out[at..at + 4].copy_from_slice(&(v.results[k] as u32).to_be_bytes());
        out[at + 4] = v.states[k];
        out[at + 5..at + 8].copy_from_slice(&[0; 3]);
        out[at + 8..at + 12].copy_from_slice(&v.detail[k][0].to_be_bytes());
        out[at + 12..at + 16].copy_from_slice(&v.detail[k][1].to_be_bytes());
        out[at + 16..at + 16 + NEEDS_LEN].copy_from_slice(&v.needs[k]);
        out[at + 16 + NEEDS_LEN..at + LOADS_ENTRY].copy_from_slice(&v.names[k]);
        at += LOADS_ENTRY;
        k += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::plan::{Reason, Verdict};

    fn fresh() -> ([u8; SEL_MAX], [[u32; 2]; SEL_MAX]) {
        ([STATE_ACTIVE; SEL_MAX], [[9; 2]; SEL_MAX])
    }

    #[test]
    fn begin_makes_every_module_selected_and_clears_the_detail() {
        let (mut s, mut d) = fresh();
        let mut b = BootStates {
            states: &mut s,
            detail: &mut d,
        };
        b.begin();
        assert_eq!(*b.states, [STATE_SELECTED; SEL_MAX]);
        assert_eq!(*b.detail, [[0; 2]; SEL_MAX]);
    }

    #[test]
    fn an_accepted_module_goes_planned_init_active() {
        let (mut s, mut d) = fresh();
        let mut b = BootStates {
            states: &mut s,
            detail: &mut d,
        };
        b.begin();
        assert!(b.planned(0, Verdict::Accepted));
        assert_eq!(b.states[0], STATE_PLANNED);
        assert!(b.init(0));
        assert_eq!(b.states[0], STATE_INIT);
        assert!(b.finished(0, OK));
        assert_eq!(b.states[0], STATE_ACTIVE);
    }

    #[test]
    fn a_module_with_a_dsp_part_goes_through_dsp_loading() {
        let (mut s, mut d) = fresh();
        let mut b = BootStates {
            states: &mut s,
            detail: &mut d,
        };
        b.begin();
        b.planned(1, Verdict::Accepted);
        b.init(1);
        assert!(b.finished(1, LOADING));
        assert_eq!(b.states[1], STATE_DSP_LOADING);
        assert!(b.dsp_done(1, OK));
        assert_eq!(b.states[1], STATE_ACTIVE);
        // a second DSP result does nothing
        assert!(!b.dsp_done(1, E_DSP_XFER));
        assert_eq!(b.states[1], STATE_ACTIVE);
    }

    #[test]
    fn a_failed_dsp_part_and_a_failed_load_are_failed() {
        let (mut s, mut d) = fresh();
        let mut b = BootStates {
            states: &mut s,
            detail: &mut d,
        };
        b.begin();
        for k in 0..2 {
            b.planned(k, Verdict::Accepted);
            b.init(k);
        }
        b.finished(0, E_INIT);
        assert_eq!(b.states[0], STATE_FAILED);
        b.finished(1, LOADING);
        assert!(b.dsp_done(1, E_DSP_ROWS));
        assert_eq!(b.states[1], STATE_FAILED);
    }

    #[test]
    fn a_refused_module_keeps_its_detail_and_never_starts() {
        let (mut s, mut d) = fresh();
        let mut b = BootStates {
            states: &mut s,
            detail: &mut d,
        };
        b.begin();
        let cases = [
            (Reason::NeedMissing(0x5eed), [0x5eed, 0]),
            (Reason::NeedVersion, [0, 0]),
            (Reason::UnownedKind(90), [90, 0]),
            (Reason::DuplicateOwner(7), [7, 0]),
            (Reason::DuplicateKey(5, 0x1234), [5, 0x1234]),
            (Reason::Fit(3), [3, 0]),
        ];
        for (k, (why, detail)) in cases.iter().enumerate() {
            assert!(b.planned(k, Verdict::Refused(*why)));
            assert_eq!(b.states[k], STATE_REFUSED, "{why:?}");
            assert_eq!(b.detail[k], *detail, "{why:?}");
            assert!(!b.init(k), "a refused module does not start");
            assert!(!b.finished(k, OK), "and cannot become active");
            assert_eq!(b.states[k], STATE_REFUSED);
        }
    }

    #[test]
    fn a_transition_from_the_wrong_state_does_nothing() {
        let (mut s, mut d) = fresh();
        let mut b = BootStates {
            states: &mut s,
            detail: &mut d,
        };
        b.begin();
        assert!(!b.init(0), "SELECTED is not PLANNED");
        assert!(!b.finished(0, OK));
        assert!(!b.dsp_done(0, OK));
        b.planned(0, Verdict::Accepted);
        assert!(
            !b.planned(0, Verdict::Refused(Reason::Fit(1))),
            "a planned module cannot be refused later"
        );
        assert_eq!(b.states[0], STATE_PLANNED);
        // an index outside the selection table
        assert!(!b.planned(SEL_MAX, Verdict::Accepted));
        assert!(!b.init(SEL_MAX));
    }

    #[test]
    fn safe_mode_skips_the_selected_modules_and_only_them() {
        let (mut s, mut d) = fresh();
        let mut b = BootStates {
            states: &mut s,
            detail: &mut d,
        };
        b.begin();
        b.skip_all(3);
        assert_eq!(
            b.states[..4],
            [STATE_SKIPPED, STATE_SKIPPED, STATE_SKIPPED, STATE_SELECTED]
        );
        b.skip_all(SEL_MAX + 4);
        assert_eq!(*b.states, [STATE_SKIPPED; SEL_MAX]);
    }

    #[test]
    fn classify_maps_each_load_code_to_one_state() {
        assert_eq!(classify(OK), STATE_ACTIVE);
        assert_eq!(classify(LOADING), STATE_DSP_LOADING);
        assert_eq!(classify(E_SAFE_MODE), STATE_SKIPPED);
        // every other code of `[load.status]` is a failure of a started load
        for code in [
            E_NOT_FOUND,
            E_FORMAT,
            E_SHA,
            E_IMPORT,
            E_NOMEM,
            E_INIT,
            E_LOADED,
            E_DECL_ID,
            E_DECL_USES,
            E_FX_ID_DUP,
            E_DSP_FORMAT,
            E_DSP_DEVICE,
            E_CONTRACT,
            E_MISSING,
        ] {
            assert_eq!(classify(code), STATE_FAILED, "{code}");
        }
        // the plan's codes are a refusal only through the planner
        for r in [
            Reason::NeedMissing(1),
            Reason::NeedVersion,
            Reason::UnownedKind(1),
            Reason::DuplicateOwner(1),
            Reason::DuplicateKey(1, 2),
            Reason::Fit(1),
        ] {
            assert_eq!(classify(r.code()), STATE_FAILED);
        }
    }

    #[test]
    fn the_loads_reply_has_the_version_2_layout() {
        let mut results = [0i32; SEL_MAX];
        let mut states = [STATE_SELECTED; SEL_MAX];
        let mut detail = [[0u32; 2]; SEL_MAX];
        let mut needs = [[0u8; NEEDS_LEN]; SEL_MAX];
        let mut names = [[0u8; NAME_LEN]; SEL_MAX];
        results[0] = OK;
        states[0] = STATE_ACTIVE;
        names[0][..8].copy_from_slice(b"pluck.dk");
        results[1] = E_PLAN_KEY;
        states[1] = STATE_REFUSED;
        detail[1] = [0x0102_0304, 0x0a0b_0c0d];
        names[1][..3].copy_from_slice(b"two");
        results[2] = E_IMPORT;
        states[2] = STATE_FAILED;
        needs[2][..8].copy_from_slice(b"usb-bulk");
        let v = LoadsView {
            count: 3,
            results: &results,
            states: &states,
            detail: &detail,
            needs: &needs,
            names: &names,
        };
        let mut out = [0xeeu8; loads_len(SEL_MAX)];
        let n = loads_v2(&v, &mut out);
        assert_eq!(n, loads_len(3));
        assert_eq!(LOADS_ENTRY, 56);
        assert_eq!(out[..2], [2, 3]);
        let e1 = &out[2 + LOADS_ENTRY..2 + 2 * LOADS_ENTRY];
        assert_eq!(e1[..4], (E_PLAN_KEY as u32).to_be_bytes());
        assert_eq!(e1[4..8], [STATE_REFUSED, 0, 0, 0]);
        assert_eq!(e1[8..16], [1, 2, 3, 4, 0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(e1[16..28], [0; 12]);
        assert_eq!(&e1[28..31], b"two");
        assert_eq!(e1[31..56], [0; 25]);
        let e2 = &out[2 + 2 * LOADS_ENTRY..n];
        assert_eq!(e2[4], STATE_FAILED);
        assert_eq!(&e2[16..24], b"usb-bulk");
        assert_eq!(&out[2..6], &[0; 4], "OK is zero");
        // a count above the table is cut to the table
        let big = LoadsView {
            count: SEL_MAX + 3,
            ..v
        };
        assert_eq!(loads_v2(&big, &mut out), loads_len(SEL_MAX));
        assert_eq!(out[1] as usize, SEL_MAX);
    }
}
