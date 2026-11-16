//! Safe mode and the stable period: the safe-mode key, and the UI-loop pass that clears the boot counter once the session has been stable
//! (`DKRT_STABLE_BLOCKS` DSP blocks, or at once in a safe boot).

use crate::boot::{info, INFO_MAGIC};
use crate::dlog;
use crate::selection::{current, update, Choice};
use core::ptr::addr_of;
use dkrt_abi::platform::Platform;
use dkrt_sdk::*;

/// The stable period in DSP blocks (`STABLE_BLOCKS`). A static so that the scenarios can shorten it (they poke it; nothing else writes it).
#[repr(C, align(4))]
pub struct Knob(pub u32); // a ColdFire gcc aligns a u32 to 2: the emulator's poke wants a word
#[no_mangle]
pub static mut DKRT_STABLE_BLOCKS: Knob = Knob(STABLE_BLOCKS);
/// The block count at the first UI-loop pass and the number of passes since (the fallback when the block counter does not move).
#[no_mangle]
pub static mut DKRT_UI_START: Knob = Knob(0);
static mut UI_PASSES: u32 = 0;
/// The boot counter a stable session leaves behind: 1 after a boot that found the crash record of a session that had been stable, else 0.
pub(crate) static mut LATE_FLOOR: u32 = 0;
/// UI-loop passes after which a block counter that has not moved at all is taken for dead and the counter is cleared as in the first
/// pass of old (the UI loop runs 30 to 120 times a second: 1800 passes are 15 s to a minute).
const DEAD_CLOCK_PASSES: u32 = 1800;

/// Is the safe-mode key down? Reads the stock key state byte. REC is key 0x13, panel channel 2 bit 2. Returns 0 before the first
/// channel-2 report. Hardware timing not yet proved **[O]**.
pub(crate) unsafe fn key_held() -> bool {
    crate::PLATFORM.key_down(SAFE_KEY_CODE)
}

/// Every pass of the UI loop (`dkrt_tick`). The first pass notes the block count; the boot counter goes back to 0 when the session has run
/// `DKRT_STABLE_BLOCKS` blocks past it, or at once in a safe boot (nothing of a module runs there). A hang or a fault in between leaves
/// the counter raised, so the next boots count it: two such boots in a row start the third in safe mode.
pub unsafe fn ui_pass() {
    let b = info();
    if b.stable != 0 {
        return;
    }
    if b.magic != INFO_MAGIC {
        b.ui_reached = 1;
        b.stable = 1;
        return; // the boot hook did not run (an image patched without the boot site)
    }
    let now = crate::fw_blocks().unwrap_or(0);
    if b.ui_reached == 0 {
        b.ui_reached = 1;
        DKRT_UI_START.0 = now;
        UI_PASSES = 0;
        dlog::ev(logk::UI_FIRST, dlog::INFO, [now, b.safe, b.loaded, 0, 0]);
        // The UI loop starts only after the boot intro: a host that waits for FW_READY may use the stock MidiRpc service from here on.
        dlog::ev(logk::FW_READY, dlog::INFO, [now, 0, 0, 0, 0]);
    }
    UI_PASSES = UI_PASSES.wrapping_add(1);
    let elapsed = now.wrapping_sub(DKRT_UI_START.0);
    let period = core::ptr::read_volatile(addr_of!(DKRT_STABLE_BLOCKS.0));
    let dead = elapsed == 0 && UI_PASSES >= DEAD_CLOCK_PASSES;
    if b.safe != 0 || elapsed >= period || dead {
        b.stable = 1;
        // The record as the card holds it now, not the one this boot started from: the menu may have saved a selection since.
        // A boot that followed a crash in a stable session leaves one bad boot counted (`LATE_FLOOR`), so that a second such crash leads to safe mode.
        let floor = LATE_FLOOR;
        // With SAFE_READ the record is unknown: the counter stays as it is.
        let cur = match current() {
            Choice::Use { record, copy, .. } if b.reason != SAFE_READ => Some((record, copy)),
            _ => None,
        };
        if let Some((mut r, copy)) = cur {
            if r.boot_counter != floor {
                r.boot_counter = floor;
                update(r, copy, true);
            }
        }
        dlog::ev(logk::STABLE, dlog::INFO, [now, floor, 0, 0, 0]);
    }
}
