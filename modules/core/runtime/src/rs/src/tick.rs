//! The UI-loop tick `dkrt_tick`: services, the mailbox, the modules' ticks and the poll functions.

use crate::api::{dkrt_load, dkrt_restart};
use crate::health::{dkrt_health, DKRT_HEALTH};
use crate::mailbox::mailbox_select;
use crate::platform::st;
use crate::state::{init_once, DKRT_MAILBOX, DKRT_POLLS, POLLS};
use crate::handlers::{services, tick_phase};
use crate::{admin_cmds, boot, card, dlog, fault, entry, files, intro, io, loader, settings, store_writer, watch, worker};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::*;

/// One tick phase of the services, measured as a span (owner `SPAN_PHASE | p`) while a span sink and clock are set.
pub(crate) unsafe fn phase(p: u32) {
    dlog::spanned(dlog::SPAN_PHASE | p, || services().tick(p));
}

/// Every registered poll function, each measured as a span (owner `SPAN_POLL | i`, `i` its slot) while a span sink and clock are set.
pub(crate) unsafe fn poll_all() {
    let mut i = 0;
    while i < POLLS {
        let f = read_volatile(addr_of!(DKRT_POLLS[i]));
        if f != 0 {
            let call: unsafe extern "C" fn() = core::mem::transmute(f);
            dlog::spanned(dlog::SPAN_POLL | i as u32, || call());
        }
        i += 1;
    }
}

/// Called from the UI task's event loop (`dkrt_loop_hook`, src/cf/tramp.s) once per pass, before it waits for the next
/// event. Registers d0, d1, a0, a1 are saved by the hook. Polls the mailbox, then calls the tick of every module.
#[no_mangle]
pub unsafe extern "C" fn dkrt_tick() {
    init_once();
    let s = st();
    s.ticks = s.ticks.wrapping_add(1);
    intro::ui_pass();
    boot::ui_pass();
    admin_cmds::restart_poll();
    store_writer::tick();
    fault::install();
    crate::link::pump();
    loader::dsp_step();
    worker::tick();
    io::tick();
    phase(tick_phase::EARLY);
    files::tick();
    phase(tick_phase::LATE);
    phase(tick_phase::LAST);
    settings::tick();
    watch::tick();
    let mb = &mut *addr_of_mut!(DKRT_MAILBOX);
    let cmd = read_volatile(addr_of!(mb.cmd));
    if cmd != 0 {
        // an if chain, not a `match`: a dense match becomes a PC-relative jump table, which is not ColdFire code
        let r = if cmd == CMD_LOAD {
            dkrt_load(mb.arg.as_ptr())
        } else if cmd == CMD_SELECT {
            mailbox_select(mb.arg.as_ptr(), 2)
        } else if cmd == CMD_SELECT_ONE {
            mailbox_select(mb.arg.as_ptr().add(1), *mb.arg.as_ptr() as u32)
        } else if cmd == CMD_RESTART {
            dkrt_restart()
        } else if cmd == CMD_HEALTH {
            dkrt_health(addr_of_mut!(DKRT_HEALTH));
            OK
        } else if cmd == CMD_CARD_PROBE {
            card::probe(mb.arg.as_ptr())
        } else if cmd >= CMD_SETTINGS_PUT && cmd <= CMD_SETTINGS_FLUSH {
            settings::mailbox(cmd, mb.arg.as_mut_ptr())
        } else {
            E_FORMAT
        };
        write_volatile(addr_of_mut!(mb.result), r);
        write_volatile(addr_of_mut!(mb.seq), mb.seq.wrapping_add(1));
        write_volatile(addr_of_mut!(mb.cmd), 0);
    }
    // the tick of every loaded module (`dk_module.tick`)
    entry::tick_all();
    poll_all();
}
