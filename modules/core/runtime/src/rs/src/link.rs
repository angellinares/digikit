//! The DSP link of the device: the queue of the crate `dkrt-link` over the transport of `core/dsp-link` (docs/plans/runtime-and-services.md, R6).
use core::ptr::addr_of_mut;
pub use dkrt_link::*;
use dkrt_abi::link::*;

// ---- the link of the device ------------------------------------------------------------------------------------------------

/// `core/dsp-link` through the loader's call wrappers.
pub struct DspTransport;

impl Transport for DspTransport {
    unsafe fn send(&mut self, win: u32, dst: u32, buf: *const u8, len: u32) -> Sent {
        // What the loader did before the queue: the area send (`dl_send`) answered -2 (retained or a bad destination) with a failure, and every other
        // refusal, a word write included, was tried again until the loader's guard ended it. A retained state therefore holds word writes; it fails area sends.
        if win == 0 {
            let r = crate::dsp::call_send(dst, buf, len);
            if r == 0 {
                Sent::Accepted
            } else if r == -1 {
                Sent::Refused
            } else {
                Sent::Hard
            }
        } else if crate::dsp::retained() {
            Sent::Refused
        } else if crate::dsp::call_send_win(win, dst, buf as *const u32, len) == 0 {
            Sent::Accepted
        } else {
            Sent::Refused
        }
    }
    unsafe fn poll(&mut self) -> Poll {
        // a retained DSP state is never polled again (nothing it owns may change until the reboot)
        if crate::dsp::retained() {
            return Poll::Busy;
        }
        let mut s = crate::dsp::DlStatus { state: 0, err: 0, nchunks: 0, acked: 0, sent: 0, resent: 0, nerr: 0 };
        crate::dsp::call_poll(&mut s);
        if s.state == crate::dsp::DL_DONE {
            Poll::Done
        } else if s.state == crate::dsp::DL_ERROR {
            crate::dsp::ds().last_dl_err = s.err;
            Poll::Error(s.err)
        } else {
            Poll::Busy
        }
    }
    fn event(&mut self, what: u32, a: [u32; 4]) {
        // one flag test without a sink
        if !crate::dlog::sink_present() && !crate::dlog::sink_on(crate::dlog::SINK_LOG) {
            return;
        }
        let flags = if what == EV_ERROR { dkrt_sdk::logk::L_WARN << dkrt_sdk::logk::LEVEL_SHIFT } else { crate::dlog::INFO };
        unsafe { crate::dlog::ev(dkrt_sdk::logk::DSP_LINK, flags, [what, a[0], a[1], a[2], a[3]]) }
    }
}

static mut LINK: Link<DspTransport> = Link::new(DspTransport);

/// The link of the device.
pub fn lk() -> &'static mut Link<DspTransport> {
    unsafe { &mut *addr_of_mut!(LINK) }
}

/// One UI pass of the link: settle the transfer in flight and start the next. `dkrt_tick` calls it before the loader and the services.
pub unsafe fn pump() {
    lk().pump()
}
