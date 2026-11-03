//! Constants from device data, for code that cannot read a file: `patchkit gen` writes them, `gen --check` keeps them current.
//!
//! The sources are the device files (`devices/*.toml`: `[board]`, `[digikit]`, `[card.store]`) and the run-time type range of
//! `core/machines` (`types.toml`). A module asks for a file with a `[[gen]]` entry in its `module.toml` (`module::GenDef`):
//!
//! - `rust`: `pub const`s for a crate that is one image for all devices (the digikit runtime). Only constants that every device file
//!   agrees on are written; a difference is an error (the fact must then become a per-device value that the runtime reads from its
//!   device object). Written file: one, with no `{dev}` in the path.
//! - `gas-shared`: the same constants as `.equ NAME, value` lines for ColdFire assembly shared by all devices (one file).
//! - `gas`: `.equ NAME, value` lines for ColdFire assembly, one file per device (`#include "device-<dev>.inc"` in a source of
//!   the module; `patchkit build` expands it, `.include` also works).
//! - `sharc`: `#define NAME value` lines for SHARC+ assembly, one file per device (decimal values: the assembler upper-cases
//!   macro text, which breaks `0x` literals after `+`).
//! - `sharc-adapter`: `#define ADM_*` lines for the machine adapter's SHARC+ assembly, one file per device, from `[adapter.machine]`
//!   and `[modules.frame]` of the device file (docs/design/module-sdk.md 5.2). Only those names, no other constant.
//! - `gas-sample-pool`: `.equ SP_*` lines for the sample pool adapter's ColdFire assembly, one file per device, from
//!   `[adapter.sample_pool]` (slot floor and the reserved DDR range; docs/design/sample-api-1.5.md 2.3). Only those names.
//! - `sharc-sample-pool`: `#define SP_DDR_BASE_NW32` and `SP_DDR_PAGES` for the sample pool's SHARC+ receiver, one file per device that owns a
//!   DDR range (a device that borrows the stock pool gets no file), from the same fields (docs/design/dn2-sample-pool.md 2.7).
//!
//! The files carry no firmware bytes, only numbers of the device files.

use crate::error::*;
use crate::module::Library;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write;

/// One generated file: path relative to the module directory, and its text.
pub type GenFile = (String, String);

#[derive(Deserialize)]
struct Doc {
    device: Dev,
    board: Option<Board>,
    digikit: Option<Digikit>,
    card: Option<Card>,
    limits: Option<Limits>,
    adapter: Option<AdapterDoc>,
    modules: Option<ModulesDoc>,
    input: Option<InputDoc>,
    panel: Option<PanelDoc>,
    fx: Option<FxDoc>,
}
/// `[fx]`: the facts of the effect pages of run-time effects (core/fxpages, docs/design/digikit-registry.md "Effect pages"): where the kit keeps its
/// effect values, which of them the firmware does not use (effect row k owns `value_indices[8k .. 8k + 8]`), the page numbering and the stock
/// parameter rows whose display an effect knob copies.
#[derive(Deserialize)]
struct FxDoc {
    /// The address of the firmware's pointer to the live kit object (an SRAM global), and the byte offset of the kit's array of s16 effect values in it.
    kit_ptr: u64,
    array_off: u64,
    /// The page number of the first effect page (the number of stock pages).
    page_first: u64,
    /// Indices of the effect value array that no stock parameter uses, in the order the effect rows take them (a multiple of 8, at most `FX_VALUES_MAX`).
    value_indices: Vec<u64>,
    /// Stock parameter codes: a 0 .. 127 knob, a -64 .. +63 knob, and the level slot (MIX).
    template_percent: u64,
    template_bipolar: u64,
    template_mix: u64,
    /// Where the kit's storage keeps the effect values (persistence): the byte offset of the first spare halfword in the storage record of the effect value
    /// array, and the number of spare halfwords. The values (one halfword for each of `value_indices`) come first, then one effect ID for each effect row.
    store_off: u64,
    store_halfwords: u64,
    /// An array index nothing uses (not one of `value_indices`): the mark that an effect row holds values (core/fxpages).
    mark_index: u64,
    /// The byte offset of the u32 after the array's halfwords (the stock loader and saver move it with `move.l (0x98,record),(tail,array)`).
    tail_off: u64,
    /// The send page (FX routing): the stock page it copies its title and level slot from, the address of the table of the 16 Sound object pointers, the stock send row
    /// whose owner and key words a send knob copies, and the Sound array indices the send knobs use (the first `[limits] send_slots` of them). All 0 / empty where the
    /// device has no send page yet.
    #[serde(default)]
    track_page: u64,
    #[serde(default)]
    sound_off: u64,
    #[serde(default)]
    sound_stride: u64,
    #[serde(default)]
    send_template: u64,
    #[serde(default)]
    send_indices: Vec<u64>,
}
/// `[input]`: which input backend the device uses (a name of `INPUT_BACKENDS`); the profile chooses, the code of core/input has one backend each.
#[derive(Deserialize)]
struct InputDoc {
    backend: String,
}
/// `[panel]`: the key codes: codes 1 to 8 * `linear_channels` are linear (code = channel * 8 + bit + 1), `[panel.exceptions]` lists the others as
/// code = [channel, bit]. Only these numbers are generated; the groups are for hosts that draw the panel.
#[derive(Deserialize)]
struct PanelDoc {
    linear_channels: u64,
    /// The channels the firmware's button decoder has tables for (it has no bounds check; docs/findings/58-panel-input-path.md): a key channel is below it.
    button_channels: u64,
    encoders: u64,
    #[serde(default)]
    exceptions: BTreeMap<String, [u64; 2]>,
}
#[derive(Deserialize)]
struct AdapterDoc {
    machine: Option<AdapterMachine>,
    sample_pool: Option<AdapterSamplePool>,
}
/// `[adapter.sample_pool]`: where the sample pool adapter puts samples. A device borrows the stock pool (a slot floor) or owns a DDR range
/// (base and bytes); the unused fields are 0.
#[derive(Deserialize)]
struct AdapterSamplePool {
    /// The lowest stock sample slot the adapter may take (it takes slots from 0x3ff down; 0: not used).
    slot_floor: u64,
    /// The reserved DDR range of an own allocator (base is a physical byte address; 0, 0: not used).
    ddr_base: u64,
    ddr_bytes: u64,
    /// The base of the 16 R rings of the stereo machines (16 x 32 KiB of the DSP's DDR; 0: no stereo rings). Outside the pool range, in DDR the stock never writes.
    #[serde(default)]
    ring_r_base: u64,
}
/// `[adapter.machine]`: the machine adapter's device facts. The strings are checked, the numbers become `ADM_*` defines.
#[derive(Deserialize)]
struct AdapterMachine {
    voices: u64,
    voice_unit: String,
    pitch_hw: u64,
    velocity_hw: u64,
    type_source: String,
    type_hw: Option<u64>,
    record_active: Option<u64>,
    record_mark: Option<u64>,
    note_on: String,
    trig_hw: Option<u64>,
    pair: String,
    tune_slot: Option<u64>,
    param_slots: Vec<u64>,
    reserved_slots: Vec<u64>,
}
#[derive(Deserialize)]
struct ModulesDoc {
    frame: Option<FrameDoc>,
}
#[derive(Deserialize)]
struct FrameDoc {
    base: u64,
    stride: u64,
    group: Option<u64>,
    gap: Option<u64>,
}
#[derive(Deserialize)]
struct Limits {
    modules: u64,
    machine_rows: u64,
    arena_bytes: u64,
    stage_bytes: u64,
    fx_slots: u64,
    send_slots: u64,
    param_rows: u64,
    page_rows: u64,
}
#[derive(Deserialize)]
struct Dev {
    short: String,
}
#[derive(Deserialize)]
struct Board {
    console_uart_base: u64,
    ddr_cached_base: u64,
    ddr_bytes: u64,
    ddr_uncached_alias: u64,
    esdhc_base: u64,
    dsp_clock_hz: u64,
    dsp_blocks_per_second: u64,
}
#[derive(Deserialize)]
struct Digikit {
    id: u64,
    name: String,
    safe_key: u64,
    dsp_area_bytes: u64,
    dsp_tail_off: u64,
    dsp_tail_words: u64,
    keep_base: u64,
    keep_bytes: u64,
    peek: Vec<[u64; 2]>,
}
#[derive(Deserialize)]
struct Card {
    store: Option<Store>,
}
#[derive(Deserialize)]
struct Store {
    first_sector: u64,
    end_sector: u64,
    selection_sectors: u64,
    settings_sectors: u64,
    header_b_sector: u64,
}

/// The device files that describe a digikit device, by short name.
#[derive(Debug, Default)]
pub struct Facts {
    docs: BTreeMap<String, Entry>,
}

#[derive(Debug)]
struct Entry {
    board: BoardF,
    digikit: DigikitF,
    store: StoreF,
    limits: LimitsF,
    adapter: Option<AdapterF>,
    sample_pool: Option<SamplePoolF>,
    input: InputF,
    fx: FxF,
}
/// A device's effect page facts (all 0 / empty on a device without run-time effect pages).
#[derive(Debug, Default)]
struct FxF {
    kit_ptr: u64,
    array_off: u64,
    page_first: u64,
    value_indices: Vec<u64>,
    template_percent: u64,
    template_bipolar: u64,
    template_mix: u64,
    store_off: u64,
    store_halfwords: u64,
    mark_index: u64,
    tail_off: u64,
    track_page: u64,
    sound_off: u64,
    sound_stride: u64,
    send_template: u64,
    send_indices: Vec<u64>,
}
/// A device's input facts: the backend (index into `INPUT_BACKENDS` plus 1; 0 none) and the panel's key code layout.
#[derive(Debug, Default)]
struct InputF {
    backend: u64,
    linear_channels: u64,
    button_channels: u64,
    encoders: u64,
    /// (code, channel, bit)
    exceptions: Vec<[u64; 3]>,
}
#[derive(Debug)]
struct SamplePoolF {
    slot_floor: u64,
    ddr_base: u64,
    ddr_bytes: u64,
    ring_r_base: u64,
}
#[derive(Debug)]
struct AdapterF {
    voices: u64,
    pitch_hw: u64,
    velocity_hw: u64,
    type_hw: Option<u64>,
    trig_hw: Option<u64>,
    record_active: Option<u64>,
    record_mark: Option<u64>,
    tune_slot: Option<u64>,
    param_slots: Vec<u64>,
    base: u64,
    stride: u64,
    group: u64,
    gap: u64,
    block_cycles: u64,
}
#[derive(Debug)]
struct LimitsF {
    modules: u64,
    machine_rows: u64,
    arena_bytes: u64,
    stage_bytes: u64,
    fx_slots: u64,
    send_slots: u64,
    // Not written into the generated files (the runtime reads the rows from core/dynreg's device object): checked against the maxima only.
    #[allow(dead_code)]
    param_rows: u64,
    #[allow(dead_code)]
    page_rows: u64,
}
#[derive(Debug)]
struct BoardF {
    console_uart_base: u64,
    ddr_cached_base: u64,
    ddr_bytes: u64,
    ddr_uncached_alias: u64,
    esdhc_base: u64,
    dsp_clock_hz: u64,
    dsp_blocks_per_second: u64,
}
#[derive(Debug)]
struct DigikitF {
    id: u64,
    name: String,
    safe_key: u64,
    dsp_area_bytes: u64,
    dsp_tail_off: u64,
    dsp_tail_words: u64,
    keep_base: u64,
    keep_bytes: u64,
    peek: Vec<[u64; 2]>,
}
#[derive(Debug)]
struct StoreF {
    first: u64,
    end: u64,
    selection_sectors: u64,
    settings_sectors: u64,
    header_b: u64,
}

impl Facts {
    pub fn from_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Result<Facts> {
        let mut docs = BTreeMap::new();
        for t in texts {
            // a sidecar file (the LED maps) has no [device]
            let Ok(d) = toml::from_str::<Doc>(t) else {
                continue;
            };
            let short = d.device.short.clone();
            let (Some(b), Some(k), Some(s), Some(l)) =
                (d.board, d.digikit, d.card.and_then(|c| c.store), d.limits)
            else {
                continue;
            };
            if k.peek.len() > PEEK_MAX {
                return Err(err!(
                    "{short}: [digikit] peek lists at most {PEEK_MAX} ranges"
                ));
            }
            if k.peek
                .iter()
                .any(|r| r[0] >= r[1] || r[0] & 3 != 0 || r[1] & 3 != 0)
            {
                return Err(err!(
                    "{short}: [digikit] peek ranges are [lo, hi) with lo < hi, both multiples of 4"
                ));
            }
            if k.keep_base & 0xfff != 0 || k.keep_bytes == 0 || k.keep_bytes & 0xfff != 0 {
                return Err(err!(
                    "{short}: [digikit] keep_base and keep_bytes must be multiples of 4 KiB"
                ));
            }
            if k.name.len() > 3 || k.name.is_empty() || !k.name.is_ascii() {
                return Err(err!(
                    "{short}: [digikit] name must be 1 to 3 ASCII characters"
                ));
            }
            check_limits(&short, &l, k.dsp_area_bytes, &crate::sdkgen::limits_max())?;
            let input = input_facts(&short, d.input, d.panel)?;
            let fx = fx_facts(&short, d.fx, l.fx_slots, l.send_slots)?;
            let (adapter_machine, sample_pool) = match d.adapter {
                Some(a) => (a.machine, a.sample_pool),
                None => (None, None),
            };
            let sample_pool = sample_pool_facts(&short, sample_pool)?;
            let adapter =
                adapter_facts(&short, adapter_machine, d.modules.and_then(|m| m.frame), &b)?;
            if docs
                .insert(
                    short.clone(),
                    Entry {
                        board: BoardF {
                            console_uart_base: b.console_uart_base,
                            ddr_cached_base: b.ddr_cached_base,
                            ddr_bytes: b.ddr_bytes,
                            ddr_uncached_alias: b.ddr_uncached_alias,
                            esdhc_base: b.esdhc_base,
                            dsp_clock_hz: b.dsp_clock_hz,
                            dsp_blocks_per_second: b.dsp_blocks_per_second,
                        },
                        digikit: DigikitF {
                            id: k.id,
                            name: k.name,
                            safe_key: k.safe_key,
                            dsp_area_bytes: k.dsp_area_bytes,
                            dsp_tail_off: k.dsp_tail_off,
                            dsp_tail_words: k.dsp_tail_words,
                            keep_base: k.keep_base,
                            keep_bytes: k.keep_bytes,
                            peek: k.peek,
                        },
                        store: StoreF {
                            first: s.first_sector,
                            end: s.end_sector,
                            selection_sectors: s.selection_sectors,
                            settings_sectors: s.settings_sectors,
                            header_b: s.header_b_sector,
                        },
                        limits: LimitsF {
                            modules: l.modules,
                            machine_rows: l.machine_rows,
                            arena_bytes: l.arena_bytes,
                            stage_bytes: l.stage_bytes,
                            fx_slots: l.fx_slots,
                            send_slots: l.send_slots,
                            param_rows: l.param_rows,
                            page_rows: l.page_rows,
                        },
                        adapter,
                        sample_pool,
                        input,
                        fx,
                    },
                )
                .is_some()
            {
                return Err(err!("two device files describe {short}"));
            }
        }
        Ok(Facts { docs })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_dir(dir: &std::path::Path) -> Result<Facts> {
        let mut texts = vec![];
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut paths: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            paths.sort();
            for p in paths {
                if p.extension().is_some_and(|e| e == "toml") {
                    texts.push(
                        std::fs::read_to_string(&p).map_err(|e| err!("{}: {e}", p.display()))?,
                    );
                }
            }
        }
        Facts::from_texts(texts.iter().map(|s| s.as_str()))
    }
}

/// A device's capacity limits against the contract maxima of `sdk/sdk.toml` `[limits.max]`: a value above its maximum is refused, and no
/// limit is 0 where the runtime needs at least one (a device without page rows says `page_rows = 0`).
fn check_limits(
    short: &str,
    l: &Limits,
    dsp_area_bytes: u64,
    max: &crate::sdkgen::LimitsMax,
) -> Result<()> {
    for (name, v, m) in [
        ("modules", l.modules, max.modules),
        ("fx_slots", l.fx_slots, max.fx_slots),
        ("send_slots", l.send_slots, max.send_slots),
        ("param_rows", l.param_rows, max.param_rows),
        ("page_rows", l.page_rows, max.page_rows),
        ("machine_rows", l.machine_rows, max.machine_rows),
        ("stage_bytes", l.stage_bytes, max.stage_bytes),
        (
            "[digikit] dsp_area_bytes",
            dsp_area_bytes,
            max.dsp_area_bytes,
        ),
    ] {
        if v > m {
            return Err(err!(
                "{short}: [limits] {name} is {v}, above the contract maximum {m} of sdk/sdk.toml [limits.max]"
            ));
        }
    }
    if l.modules == 0 {
        return Err(err!("{short}: [limits] modules must be at least 1"));
    }
    Ok(())
}

/// Parameter slots a machine can have (the contract's `DK_MAX_PARAMS`).
const ADM_SLOTS: usize = 8;
/// The `ADM_PARAM_SLOT*` value of a parameter without a slot.
const ADM_NO_SLOT: i64 = 255;

/// The input backends core/input has code for. A device file names one in `[input] backend`; a new device with the same panel path needs
/// only that line, a different path adds a backend here and in core/input.
pub const INPUT_BACKENDS: [&str; 1] = ["panel-uart"];
/// The most exceptions to the linear key codes a device file may list.
const PANEL_EXC_MAX: usize = 8;
/// Entries of `[fx] send_indices` the generated files carry (the cells of the send page; the contract maximum of `send_slots`).
const FX_SEND_MAX: usize = 2;
/// Entries of `[fx] value_indices` the generated files carry (`DK_FX_MAX` effects of 8 values).
const FX_VALUES_MAX: usize = 32;

/// Check `[fx]`: a multiple of 8 distinct indices (below 256), enough for the device's effect slots.
fn fx_facts(short: &str, f: Option<FxDoc>, fx_slots: u64, send_slots: u64) -> Result<FxF> {
    let Some(f) = f else {
        return Ok(FxF::default());
    };
    let n = f.value_indices.len();
    if n % 8 != 0 || n > FX_VALUES_MAX || (n as u64) < 8 * fx_slots {
        return Err(err!(
            "{short}: [fx] value_indices must list a multiple of 8 indices, at least 8 for each of the {fx_slots} effect slots, at most {FX_VALUES_MAX}"
        ));
    }
    // the send page: a device that has one lists an index (a Sound array index below 256, distinct) for each of its send slots
    if f.track_page != 0 {
        let mut seen = std::collections::BTreeSet::new();
        if (f.send_indices.len() as u64) < send_slots
            || f.send_indices.len() > FX_SEND_MAX
            || f.send_indices.iter().any(|i| *i > 255 || !seen.insert(*i))
            || f.sound_stride == 0
            || f.sound_off + 16 * f.sound_stride > f.array_off
            || f.send_template > 0xffff
        {
            return Err(err!(
                "{short}: [fx] send_indices must list at least [limits] send_slots ({send_slots}) and at most {FX_SEND_MAX} distinct numbers below 256; send_template a parameter code; the 16 Sound objects (sound_off, sound_stride) end before array_off"
            ));
        }
    } else if !f.send_indices.is_empty() {
        return Err(err!("{short}: [fx] send_indices needs track_page"));
    }
    let mut seen = std::collections::BTreeSet::new();
    if f.value_indices.iter().any(|i| *i > 255 || !seen.insert(*i)) {
        return Err(err!(
            "{short}: [fx] value_indices are distinct numbers below 256"
        ));
    }
    if f.page_first == 0 || f.page_first > 255 || f.array_off & 1 != 0 || f.kit_ptr & 3 != 0 {
        return Err(err!(
            "{short}: [fx] page_first is 1 to 255, array_off is even and kit_ptr a multiple of 4"
        ));
    }
    // the storage: the values, then one ID for each effect row, in halfwords the stock firmware ignores
    if f.store_off & 1 != 0 || f.store_off > 0xffff || (n + n / 8) as u64 > f.store_halfwords {
        return Err(err!(
            "{short}: [fx] store_off is even and store_halfwords holds the {n} values and {} effect IDs ({} halfwords)",
            n / 8,
            n + n / 8
        ));
    }
    if f.tail_off & 1 != 0 || f.tail_off > 0xffff || f.mark_index * 2 + 2 > f.tail_off {
        return Err(err!(
            "{short}: [fx] tail_off is even and behind the halfword of mark_index"
        ));
    }
    if f.mark_index > 255 || f.value_indices.contains(&f.mark_index) {
        return Err(err!(
            "{short}: [fx] mark_index is an array index below 256 that is not one of value_indices"
        ));
    }
    for (name, t) in [
        ("template_percent", f.template_percent),
        ("template_bipolar", f.template_bipolar),
        ("template_mix", f.template_mix),
    ] {
        if t > 0xffff {
            return Err(err!("{short}: [fx] {name} is a parameter code"));
        }
    }
    Ok(FxF {
        kit_ptr: f.kit_ptr,
        array_off: f.array_off,
        page_first: f.page_first,
        value_indices: f.value_indices,
        template_percent: f.template_percent,
        template_bipolar: f.template_bipolar,
        template_mix: f.template_mix,
        store_off: f.store_off,
        store_halfwords: f.store_halfwords,
        mark_index: f.mark_index,
        tail_off: f.tail_off,
        track_page: f.track_page,
        sound_off: f.sound_off,
        sound_stride: f.sound_stride,
        send_template: f.send_template,
        send_indices: f.send_indices,
    })
}

fn input_facts(short: &str, i: Option<InputDoc>, p: Option<PanelDoc>) -> Result<InputF> {
    let Some(i) = i else {
        return Ok(InputF::default());
    };
    let Some(backend) = INPUT_BACKENDS.iter().position(|b| *b == i.backend) else {
        return Err(err!(
            "{short}: [input] backend {:?} is not one of {INPUT_BACKENDS:?}",
            i.backend
        ));
    };
    let Some(p) = p else {
        return Err(err!("{short}: [input] needs a [panel] (the key codes)"));
    };
    if p.exceptions.len() > PANEL_EXC_MAX {
        return Err(err!(
            "{short}: [panel.exceptions] lists at most {PANEL_EXC_MAX} codes"
        ));
    }
    if p.linear_channels > p.button_channels
        || p.button_channels > 16
        || p.encoders == 0
        || p.encoders > 16
    {
        return Err(err!(
            "{short}: [panel] linear_channels <= button_channels <= 16 and encoders 1 to 16"
        ));
    }
    let mut exceptions = vec![];
    for (code, [ch, bit]) in &p.exceptions {
        let code: u64 = code
            .parse()
            .map_err(|_| err!("{short}: [panel.exceptions] key {code:?} is not a code"))?;
        if code <= p.linear_channels * 8
            || *bit > 7
            || *ch < p.linear_channels
            || *ch >= p.button_channels
        {
            return Err(err!(
                "{short}: [panel.exceptions] {code} = [{ch}, {bit}]: a code above the linear ones, on a channel after them and below button_channels, bit 0 to 7"
            ));
        }
        exceptions.push([code, *ch, *bit]);
    }
    Ok(InputF {
        backend: backend as u64 + 1,
        linear_channels: p.linear_channels,
        button_channels: p.button_channels,
        encoders: p.encoders,
        exceptions,
    })
}

/// Check `[adapter.machine]` and join it with `[modules.frame]` and the block length of `[board]`.
fn adapter_facts(
    short: &str,
    m: Option<AdapterMachine>,
    f: Option<FrameDoc>,
    b: &Board,
) -> Result<Option<AdapterF>> {
    let Some(m) = m else {
        return Ok(None);
    };
    let one_of = |what: &str, v: &str, ok: &[&str]| -> Result<()> {
        if ok.contains(&v) {
            Ok(())
        } else {
            Err(err!(
                "{short}: [adapter.machine] {what} is {v:?}, expected one of {ok:?}"
            ))
        }
    };
    one_of("voice_unit", &m.voice_unit, &["track", "slot"])?;
    one_of("type_source", &m.type_source, &["cache", "frame"])?;
    one_of("note_on", &m.note_on, &["record", "trig-edge"])?;
    one_of("pair", &m.pair, &["copy-even", "none"])?;
    let need = |what: &str, v: Option<u64>, when: &str| -> Result<()> {
        if v.is_some() {
            Ok(())
        } else {
            Err(err!(
                "{short}: [adapter.machine] {what} is needed for {when}"
            ))
        }
    };
    if m.note_on == "record" {
        need("record_active", m.record_active, "note_on = \"record\"")?;
        need("record_mark", m.record_mark, "note_on = \"record\"")?;
    }
    if m.note_on == "trig-edge" {
        need("trig_hw", m.trig_hw, "note_on = \"trig-edge\"")?;
    }
    if m.type_source == "frame" {
        need("type_hw", m.type_hw, "type_source = \"frame\"")?;
    }
    if m.param_slots.len() > ADM_SLOTS {
        return Err(err!(
            "{short}: [adapter.machine] param_slots lists at most {ADM_SLOTS} slots"
        ));
    }
    if m.param_slots
        .iter()
        .chain(&m.reserved_slots)
        .any(|s| *s >= ADM_SLOTS as u64)
    {
        return Err(err!(
            "{short}: [adapter.machine] param_slots and reserved_slots are knob positions 0 to {}",
            ADM_SLOTS - 1
        ));
    }
    if m.param_slots.iter().any(|s| m.reserved_slots.contains(s)) {
        return Err(err!(
            "{short}: [adapter.machine] a slot is in both param_slots and reserved_slots"
        ));
    }
    if m.voices == 0 || m.voices > 16 {
        return Err(err!("{short}: [adapter.machine] voices is 1 to 16"));
    }
    let f = f.ok_or_else(|| err!("{short}: [adapter.machine] needs [modules.frame]"))?;
    Ok(Some(AdapterF {
        voices: m.voices,
        pitch_hw: m.pitch_hw,
        velocity_hw: m.velocity_hw,
        type_hw: m.type_hw,
        trig_hw: m.trig_hw,
        record_active: m.record_active,
        record_mark: m.record_mark,
        tune_slot: m.tune_slot,
        param_slots: m.param_slots,
        base: f.base,
        stride: f.stride,
        group: f.group.unwrap_or(0),
        gap: f.gap.unwrap_or(0),
        block_cycles: (b.dsp_clock_hz + b.dsp_blocks_per_second / 2) / b.dsp_blocks_per_second,
    }))
}

/// Check `[adapter.sample_pool]`: a stock slot floor or a DDR range of the DSP's DDR, not both.
fn sample_pool_facts(short: &str, p: Option<AdapterSamplePool>) -> Result<Option<SamplePoolF>> {
    let Some(p) = p else {
        return Ok(None);
    };
    if p.slot_floor != 0 && (p.slot_floor > 0x3ff || p.slot_floor & 0x7f == 0) {
        return Err(err!(
            "{short}: [adapter.sample_pool] slot_floor is 0 or a stock slot 1 to 0x3ff that is not a multiple of 0x80"
        ));
    }
    if (p.ddr_base == 0) != (p.ddr_bytes == 0) {
        return Err(err!(
            "{short}: [adapter.sample_pool] ddr_base and ddr_bytes are both 0 or both set"
        ));
    }
    if p.ddr_bytes != 0 {
        if p.ddr_base & 0xfff != 0
            || p.ddr_bytes & 0xfff != 0
            || p.ddr_base < DSP_DDR_BASE
            || p.ddr_base
                .checked_add(p.ddr_bytes)
                .is_none_or(|e| e > 0x1_0000_0000)
        {
            return Err(err!(
                "{short}: [adapter.sample_pool] the DDR range must be 4 KiB aligned, in the DSP's DDR (from 0x80000000) and below 4 GiB"
            ));
        }
    }
    if p.ddr_bytes != 0 && (p.ddr_base < SAMPLE_POOL_STOCK_END || p.ddr_base + p.ddr_bytes > DSP_DDR_END) {
        return Err(err!(
            "{short}: [adapter.sample_pool] the DDR range must start at or above 0x80531000 (the end of the stock writes seen, finding 106) and end at or below 0xa0000000 (the DSP's NW32 alias)"
        ));
    }
    if p.slot_floor != 0 && p.ddr_bytes != 0 {
        return Err(err!(
            "{short}: [adapter.sample_pool] a device borrows the stock pool or owns a range, not both"
        ));
    }
    if p.ring_r_base != 0 {
        let end = p.ring_r_base.checked_add(RING_R_BYTES);
        let overlaps = p.ddr_bytes != 0 && p.ring_r_base < p.ddr_base + p.ddr_bytes && p.ddr_base < p.ring_r_base + RING_R_BYTES;
        if p.ring_r_base & 0xfff != 0
            || p.ring_r_base < SAMPLE_POOL_STOCK_END
            || end.is_none_or(|e| e > DSP_DDR_END)
        {
            return Err(err!(
                "{short}: [adapter.sample_pool] ring_r_base must be 4 KiB aligned, start at or above 0x80531000 (the end of the stock writes seen, finding 106) and end at or below 0xa0000000"
            ));
        }
        if overlaps {
            return Err(err!(
                "{short}: [adapter.sample_pool] the 512 KiB of the R rings at ring_r_base overlap the pool range (ddr_base, ddr_bytes)"
            ));
        }
    }
    Ok(Some(SamplePoolF {
        slot_floor: p.slot_floor,
        ddr_base: p.ddr_base,
        ddr_bytes: p.ddr_bytes,
        ring_r_base: p.ring_r_base,
    }))
}

/// The first byte of the DSP's DDR in its physical byte addresses (docs/findings/99). It is not the ColdFire's DDR of `[board]`.
const DSP_DDR_BASE: u64 = 0x8000_0000;
/// The end of the DSP's DDR that the NW32 alias reaches (physical 0xa0000000).
const DSP_DDR_END: u64 = 0xa000_0000;
/// The R rings of the stereo machines: 16 rings of 32 KiB, like the stock rings (finding 106) but in the DSP's own DDR range.
const RING_R_BYTES: u64 = 16 * 0x8000;
/// The end of the last stock write seen in the DSP's DDR (finding 106 canary map): a range the adapter owns starts at or above it.
const SAMPLE_POOL_STOCK_END: u64 = 0x8053_1000;

/// The `SP_*` constants of one device.
fn sample_pool_constants(p: &SamplePoolF) -> Vec<C> {
    let v = |name, value: u64, note| c(name, value, Scope::PerDevice, note);
    vec![
        v(
            "SP_SLOT_FLOOR",
            p.slot_floor,
            "[adapter.sample_pool] slot_floor (lowest stock slot the adapter takes; 0: not used)",
        ),
        v(
            "SP_DDR_BASE",
            p.ddr_base,
            "[adapter.sample_pool] ddr_base (reserved DDR range; 0: not used)",
        ),
        v(
            "SP_DDR_BYTES",
            p.ddr_bytes,
            "[adapter.sample_pool] ddr_bytes",
        ),
    ]
}

/// The DSP's view of the same range: the NW32 word address of its first byte and its size in 4 KiB pages.
fn sample_pool_dsp_constants(p: &SamplePoolF) -> Vec<C> {
    let v = |name, value: u64, note| c(name, value, Scope::PerDevice, note);
    vec![
        v(
            "SP_DDR_BASE_NW32",
            0x1000_0000 + (p.ddr_base - DSP_DDR_BASE) / 4,
            "NW32 word address of ddr_base (0x10000000 + (ddr_base - 0x80000000) / 4)",
        ),
        v(
            "SP_DDR_PAGES",
            p.ddr_bytes / 4096,
            "[adapter.sample_pool] ddr_bytes in 4 KiB pages: the page index of a wire tag must be below it",
        ),
    ]
}

const ADM_SLOT_NAMES: [&str; ADM_SLOTS] = [
    "ADM_PARAM_SLOT0",
    "ADM_PARAM_SLOT1",
    "ADM_PARAM_SLOT2",
    "ADM_PARAM_SLOT3",
    "ADM_PARAM_SLOT4",
    "ADM_PARAM_SLOT5",
    "ADM_PARAM_SLOT6",
    "ADM_PARAM_SLOT7",
];

/// The `ADM_*` constants of one device (what the machine adapter's assembly needs; names absent on a device are not written).
fn adapter_constants(a: &AdapterF) -> Vec<C> {
    let p = |name, value: u64, note| c(name, value, Scope::PerDevice, note);
    let mut v = vec![
        p("ADM_VOICES", a.voices, "[adapter.machine] voices"),
        p(
            "ADM_PITCH_HW",
            a.pitch_hw,
            "[adapter.machine] pitch_hw (frame halfword of voice 0's pitch, note << 8; voice n is + n)",
        ),
        p(
            "ADM_VELOCITY_HW",
            a.velocity_hw,
            "[adapter.machine] velocity_hw (frame halfword of voice 0's velocity, velocity << 8; voice n is + n)",
        ),
    ];
    if let Some(x) = a.type_hw {
        v.push(p(
            "ADM_TYPE_HW",
            x,
            "[adapter.machine] type_hw (frame halfword of slot 0's machine type; slot n is + n)",
        ));
    }
    if let Some(x) = a.trig_hw {
        v.push(p(
            "ADM_TRIG_HW",
            x,
            "[adapter.machine] trig_hw (frame halfword whose bit n is slot n's trig)",
        ));
    }
    if let Some(x) = a.record_active {
        v.push(p(
            "ADM_RECORD_ACTIVE",
            x,
            "[adapter.machine] record_active (byte offset in the voice record)",
        ));
    }
    if let Some(x) = a.record_mark {
        v.push(p(
            "ADM_RECORD_MARK",
            x,
            "[adapter.machine] record_mark (byte offset in the voice record)",
        ));
    }
    if let Some(x) = a.tune_slot {
        v.push(p(
            "ADM_TUNE_SLOT",
            x,
            "[adapter.machine] tune_slot (knob position of the stock tune, folded into the pitch)",
        ));
    }
    v.extend([
        p(
            "ADM_PARAM_BASE",
            a.base,
            "[modules.frame] base (halfword of knob 0 of voice 0)",
        ),
        p(
            "ADM_PARAM_STRIDE",
            a.stride,
            "[modules.frame] stride (halfwords from one voice's knobs to the next)",
        ),
        p(
            "ADM_PARAM_GROUP",
            a.group,
            "[modules.frame] group (knobs per group; 0: no groups)",
        ),
        p(
            "ADM_PARAM_GAP",
            a.gap,
            "[modules.frame] gap (halfwords between groups)",
        ),
        p(
            "ADM_NPARAM_SLOTS",
            a.param_slots.len() as u64,
            "[adapter.machine] param_slots: number of knob positions for module parameters",
        ),
    ]);
    for (i, name) in ADM_SLOT_NAMES.iter().enumerate() {
        v.push(C {
            name: *name,
            value: a.param_slots.get(i).map_or(ADM_NO_SLOT, |s| *s as i64),
            scope: Scope::PerDevice,
            note: "[adapter.machine] param_slots: knob position of module parameter i (255: none)",
        });
    }
    v.push(p(
        "ADM_BLOCK_CYCLES",
        a.block_cycles,
        "[board] dsp_clock_hz / dsp_blocks_per_second, rounded (DSP_BLOCK_CYCLES)",
    ));
    v
}

/// Ranges of RAM the PEEK command may read, at most (a device lists fewer; the rest are 0, 0).
const PEEK_MAX: usize = 4;

/// Is a constant the same on every device (it may go into a runtime that is one image for all devices)?
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Shared,
    PerDevice,
}

struct C {
    name: &'static str,
    value: i64,
    scope: Scope,
    note: &'static str,
}

fn c(name: &'static str, value: u64, scope: Scope, note: &'static str) -> C {
    C {
        name,
        value: value as i64,
        scope,
        note,
    }
}

/// The run-time machine type range of a device: `dynamic_first ..= last` of `core/machines` types.toml.
pub fn dyn_range(lib: &Library, dev: &str) -> Result<(i64, i64)> {
    let d = lib
        .get("core/machines")
        .and_then(|m| m.alloc.get("machines"))
        .and_then(|a| a.devices.get(dev))
        .ok_or_else(|| err!("{dev}: no allocation table (core/machines types.toml)"))?;
    let first = d
        .dynamic_first
        .ok_or_else(|| err!("{dev}: types.toml has no dynamic_first"))?;
    Ok((first, d.last))
}

/// The constants of one device.
fn constants(lib: &Library, dev: &str, e: &Entry) -> Result<Vec<C>> {
    use Scope::*;
    let (first, last) = dyn_range(lib, dev)?;
    let s = &e.store;
    if s.selection_sectors != 2 {
        return Err(err!(
            "{dev}: [card.store] selection_sectors must be 2 (the runtime writes two copies of the selection record)"
        ));
    }
    let set = crate::sdkgen::settings();
    if s.settings_sectors != u64::from(set.copies) * u64::from(set.sectors) {
        return Err(err!(
            "{dev}: [card.store] settings_sectors must be {} (sdk/sdk.toml [settings]: {} copies of {} sectors)",
            u64::from(set.copies) * u64::from(set.sectors),
            set.copies,
            set.sectors
        ));
    }
    if s.header_b != s.end - s.selection_sectors - s.settings_sectors - 1 {
        return Err(err!(
            "{dev}: [card.store] header_b_sector must be the sector before the settings record, which lies before the selection record"
        ));
    }
    let b = &e.board;
    let k = &e.digikit;
    let n = (last - first + 1) as u64;
    if e.limits.machine_rows != n {
        return Err(err!(
            "{dev}: [limits] machine_rows is {}, but core/machines types.toml reserves {n} run-time machine types (dynamic_first ..= last)",
            e.limits.machine_rows
        ));
    }
    let name_word = k
        .name
        .bytes()
        .chain(std::iter::repeat(0))
        .take(4)
        .fold(0u64, |a, x| (a << 8) | x as u64);
    let mut v = vec![
        c("DEVICE_ID", k.id, PerDevice, "[digikit] id"),
        c(
            "DEVICE_NAME_WORD",
            name_word,
            PerDevice,
            "[digikit] name, NUL padded, as a big-endian word",
        ),
        c(
            "STORE_FIRST",
            s.first,
            PerDevice,
            "[card.store] first_sector",
        ),
        c(
            "STORE_SECTORS",
            s.end - s.first,
            PerDevice,
            "[card.store] end_sector - first_sector",
        ),
        c(
            "STORE_HEADER_B",
            s.header_b - s.first,
            PerDevice,
            "[card.store] header_b_sector - first_sector (store header copy B, relative to the store)",
        ),
        c(
            "STORE_SEL_A",
            s.end - 2,
            PerDevice,
            "[card.store] end_sector - 2 (selection record copy A, absolute)",
        ),
        c(
            "STORE_SEL_B",
            s.end - 1,
            PerDevice,
            "[card.store] end_sector - 1 (selection record copy B, absolute)",
        ),
        c(
            "DYN_TYPE_FIRST",
            first as u64,
            Shared,
            "core/machines types.toml dynamic_first",
        ),
        c(
            "DYN_TYPE_LAST",
            last as u64,
            Shared,
            "core/machines types.toml last",
        ),
        c(
            "DYN_TYPES",
            n,
            Shared,
            "run-time machine types; core/dynreg reserves one machine row and core/dyndsp one voice row for each",
        ),
        c(
            "CONSOLE_UART_BASE",
            b.console_uart_base,
            Shared,
            "[board] console_uart_base",
        ),
        c(
            "DDR_CACHED_BASE",
            b.ddr_cached_base,
            Shared,
            "[board] ddr_cached_base",
        ),
        c("DDR_BYTES", b.ddr_bytes, Shared, "[board] ddr_bytes"),
        c(
            "DDR_UNCACHED_ALIAS",
            b.ddr_uncached_alias,
            Shared,
            "[board] ddr_uncached_alias",
        ),
        c("ESDHC_BASE", b.esdhc_base, Shared, "[board] esdhc_base"),
        c(
            "SAFE_KEY_CODE",
            k.safe_key,
            Shared,
            "[digikit] safe_key (a panel control code)",
        ),
        c(
            "DSP_BLOCK_CYCLES",
            (b.dsp_clock_hz + b.dsp_blocks_per_second / 2) / b.dsp_blocks_per_second,
            Shared,
            "[board] dsp_clock_hz / dsp_blocks_per_second, rounded (the clock is open, finding 41)",
        ),
        c(
            "DL_AREA_BYTES",
            k.dsp_area_bytes,
            PerDevice,
            "[digikit] dsp_area_bytes",
        ),
        c(
            "DL_AREA_WORDS",
            k.dsp_area_bytes / 4,
            PerDevice,
            "[digikit] dsp_area_bytes / 4",
        ),
        c(
            "DL_TAIL_OFF",
            k.dsp_tail_off,
            PerDevice,
            "[digikit] dsp_tail_off",
        ),
        c(
            "DL_TAIL_WORDS",
            k.dsp_tail_words,
            PerDevice,
            "[digikit] dsp_tail_words",
        ),
        c(
            "KEEP_BASE",
            k.keep_base,
            Shared,
            "[digikit] keep_base (RAM a soft reset is assumed to keep: the crash record)",
        ),
        c("KEEP_BYTES", k.keep_bytes, Shared, "[digikit] keep_bytes"),
        c(
            "LIMIT_MODULES",
            e.limits.modules,
            Shared,
            "[limits] modules (module records)",
        ),
        c(
            "LIMIT_MACHINE_ROWS",
            e.limits.machine_rows,
            Shared,
            "[limits] machine_rows (run-time machine and voice rows)",
        ),
        c(
            "LIMIT_ARENA_BYTES",
            e.limits.arena_bytes,
            Shared,
            "[limits] arena_bytes (the module arena)",
        ),
        c(
            "LIMIT_FX_SLOTS",
            e.limits.fx_slots,
            PerDevice,
            "[limits] fx_slots (effects in the chain: CAPACITY's limit and the free effect rows of core/dynfx)",
        ),
        c(
            "LIMIT_STAGE_BYTES",
            e.limits.stage_bytes,
            Shared,
            "[limits] stage_bytes (the staging buffer of a .dkm file)",
        ),
        c(
            "PEEK_N",
            k.peek.len() as u64,
            PerDevice,
            "[digikit] peek: number of ranges",
        ),
    ];
    let inp = &e.input;
    v.push(c(
        "INPUT_BACKEND",
        inp.backend,
        PerDevice,
        "[input] backend (1 panel-uart; 0 none)",
    ));
    v.push(c(
        "PANEL_LINEAR",
        inp.linear_channels,
        PerDevice,
        "[panel] linear_channels (code = channel * 8 + bit + 1)",
    ));
    v.push(c(
        "PANEL_BUTTON_CHANNELS",
        inp.button_channels,
        PerDevice,
        "[panel] button_channels (the button decoder's tables: a key channel is below it)",
    ));
    v.push(c(
        "PANEL_ENCODERS",
        inp.encoders,
        PerDevice,
        "[panel] encoders",
    ));
    v.push(c(
        "PANEL_EXC_N",
        inp.exceptions.len() as u64,
        PerDevice,
        "[panel.exceptions]: number of codes",
    ));
    for k in 0..PANEL_EXC_MAX {
        let [code, ch, bit] = inp.exceptions.get(k).copied().unwrap_or([0, 0, 0]);
        v.push(C {
            name: PANEL_EXC_CODE[k],
            value: code as i64,
            scope: PerDevice,
            note: "[panel.exceptions] code",
        });
        v.push(C {
            name: PANEL_EXC_CH[k],
            value: ch as i64,
            scope: PerDevice,
            note: "[panel.exceptions] channel of the code",
        });
        v.push(C {
            name: PANEL_EXC_BIT[k],
            value: bit as i64,
            scope: PerDevice,
            note: "[panel.exceptions] bit of the code",
        });
    }
    let fx = &e.fx;
    for (name, value, note) in [
        (
            "FX_KIT_PTR",
            fx.kit_ptr,
            "[fx] kit_ptr (the firmware's pointer to the live kit object)",
        ),
        (
            "FX_ARRAY_OFF",
            fx.array_off,
            "[fx] array_off (the kit's array of effect values, bytes from the kit object)",
        ),
        (
            "FX_PAGE_FIRST",
            fx.page_first,
            "[fx] page_first (the page number of the first effect page)",
        ),
        (
            "FX_TPL_PERCENT",
            fx.template_percent,
            "[fx] template_percent (a 0 .. 127 knob)",
        ),
        (
            "FX_TPL_BIPOLAR",
            fx.template_bipolar,
            "[fx] template_bipolar (a -64 .. +63 knob)",
        ),
        (
            "FX_TPL_MIX",
            fx.template_mix,
            "[fx] template_mix (the level slot: MIX)",
        ),
        (
            "FX_STORE_OFF",
            fx.store_off,
            "[fx] store_off (the first spare halfword of the kit storage's effect value record, bytes from its start)",
        ),
        (
            "FX_STORE_HW",
            fx.store_halfwords,
            "[fx] store_halfwords (spare halfwords in that record: the values, then one effect ID for each effect row)",
        ),
        (
            "FX_MARK_INDEX",
            fx.mark_index,
            "[fx] mark_index (the array index that holds the mark: an effect row has its values)",
        ),
        (
            "FX_TAIL_OFF",
            fx.tail_off,
            "[fx] tail_off (the u32 after the array's halfwords: the loader and saver copy it, bytes from the array)",
        ),
        (
            "FX_VALUES_N",
            fx.value_indices.len() as u64,
            "[fx] value_indices: number of indices",
        ),
        (
            "FX_TRACK_PAGE",
            fx.track_page,
            "[fx] track_page (the stock page the send page copies its title and level slot from; 0: no send page)",
        ),
        (
            "FX_SOUND_OFF",
            fx.sound_off,
            "[fx] sound_off (the first Sound object in the kit object, bytes from the kit object)",
        ),
        (
            "FX_SOUND_STRIDE",
            fx.sound_stride,
            "[fx] sound_stride (bytes between the 16 Sound objects)",
        ),
        (
            "FX_SEND_TEMPLATE",
            fx.send_template,
            "[fx] send_template (the stock send row whose owner and key words a send knob copies)",
        ),
        (
            "LIMIT_SEND_SLOTS",
            if fx.track_page == 0 { 0 } else { e.limits.send_slots },
            "[limits] send_slots (effects on a send bus: the cells of the send page; 0 on a device without one)",
        ),
    ] {
        v.push(c(name, value, PerDevice, note));
    }
    for (i, name) in FX_VALUE.iter().enumerate() {
        v.push(c(
            name,
            fx.value_indices.get(i).copied().unwrap_or(0),
            PerDevice,
            "[fx] value_indices (an effect value index that no stock parameter uses)",
        ));
    }
    for (i, name) in FX_SEND_IDX.iter().enumerate() {
        v.push(c(
            name,
            fx.send_indices.get(i).copied().unwrap_or(0),
            PerDevice,
            "[fx] send_indices (a Sound array index the send page's knob uses)",
        ));
    }
    for i in 0..PEEK_MAX {
        let r = k.peek.get(i).copied().unwrap_or([0, 0]);
        v.push(C {
            name: PEEK_LO[i],
            value: r[0] as i64,
            scope: PerDevice,
            note: "[digikit] peek range start (RAM the PEEK command may read)",
        });
        v.push(C {
            name: PEEK_HI[i],
            value: r[1] as i64,
            scope: PerDevice,
            note: "[digikit] peek range end (exclusive)",
        });
    }
    Ok(v)
}

const PANEL_EXC_CODE: [&str; PANEL_EXC_MAX] = [
    "PANEL_EXC_CODE_0",
    "PANEL_EXC_CODE_1",
    "PANEL_EXC_CODE_2",
    "PANEL_EXC_CODE_3",
    "PANEL_EXC_CODE_4",
    "PANEL_EXC_CODE_5",
    "PANEL_EXC_CODE_6",
    "PANEL_EXC_CODE_7",
];
const PANEL_EXC_CH: [&str; PANEL_EXC_MAX] = [
    "PANEL_EXC_CH_0",
    "PANEL_EXC_CH_1",
    "PANEL_EXC_CH_2",
    "PANEL_EXC_CH_3",
    "PANEL_EXC_CH_4",
    "PANEL_EXC_CH_5",
    "PANEL_EXC_CH_6",
    "PANEL_EXC_CH_7",
];
const PANEL_EXC_BIT: [&str; PANEL_EXC_MAX] = [
    "PANEL_EXC_BIT_0",
    "PANEL_EXC_BIT_1",
    "PANEL_EXC_BIT_2",
    "PANEL_EXC_BIT_3",
    "PANEL_EXC_BIT_4",
    "PANEL_EXC_BIT_5",
    "PANEL_EXC_BIT_6",
    "PANEL_EXC_BIT_7",
];
const FX_SEND_IDX: [&str; FX_SEND_MAX] = ["FX_SEND_IDX_0", "FX_SEND_IDX_1"];
const FX_VALUE: [&str; FX_VALUES_MAX] = [
    "FX_VALUE_0",
    "FX_VALUE_1",
    "FX_VALUE_2",
    "FX_VALUE_3",
    "FX_VALUE_4",
    "FX_VALUE_5",
    "FX_VALUE_6",
    "FX_VALUE_7",
    "FX_VALUE_8",
    "FX_VALUE_9",
    "FX_VALUE_10",
    "FX_VALUE_11",
    "FX_VALUE_12",
    "FX_VALUE_13",
    "FX_VALUE_14",
    "FX_VALUE_15",
    "FX_VALUE_16",
    "FX_VALUE_17",
    "FX_VALUE_18",
    "FX_VALUE_19",
    "FX_VALUE_20",
    "FX_VALUE_21",
    "FX_VALUE_22",
    "FX_VALUE_23",
    "FX_VALUE_24",
    "FX_VALUE_25",
    "FX_VALUE_26",
    "FX_VALUE_27",
    "FX_VALUE_28",
    "FX_VALUE_29",
    "FX_VALUE_30",
    "FX_VALUE_31",
];
const PEEK_LO: [&str; PEEK_MAX] = ["PEEK_LO_0", "PEEK_LO_1", "PEEK_LO_2", "PEEK_LO_3"];
const PEEK_HI: [&str; PEEK_MAX] = ["PEEK_HI_0", "PEEK_HI_1", "PEEK_HI_2", "PEEK_HI_3"];

fn hex(v: i64) -> String {
    format!("{v:#x}")
}

const NOTE: &str =
    "generated by `patchkit gen` from the device files and core/machines types.toml; do not edit";

/// The shared constants, checked to be equal on every device.
fn shared<'a>(module: &str, per_dev: &'a [(String, Vec<C>)]) -> Result<Vec<&'a C>> {
    let (first_dev, first) = &per_dev[0];
    let mut out = vec![];
    for (i, k) in first.iter().enumerate() {
        if k.scope != Scope::Shared {
            continue;
        }
        for (dev, other) in &per_dev[1..] {
            if other[i].value != k.value {
                return Err(err!(
                    "{module}: {} is {} on {first_dev} but {} on {dev}; the runtime is one image for all devices, so the fact must become a per-device value that it reads from its device object",
                    k.name,
                    hex(k.value),
                    hex(other[i].value)
                ));
            }
        }
        out.push(k);
    }
    Ok(out)
}

fn rust(module: &str, per_dev: &[(String, Vec<C>)]) -> Result<String> {
    let mut s = format!(
        "// {module}: {NOTE}.\n// Only constants that every device file agrees on; the runtime is one image for all devices.\n\n"
    );
    for k in shared(module, per_dev)? {
        let _ = writeln!(s, "/// {}", k.note);
        let _ = writeln!(s, "pub const {}: u32 = {};", k.name, hex(k.value));
    }
    Ok(s)
}

/// A C header of the contract maxima of `sdk/sdk.toml` `[limits.max]` that a ColdFire object shared by all devices checks against: the
/// DSP area's size is a device value, but the link's word offset (17 bits) is the contract's.
fn c_shared(module: &str) -> String {
    let max = crate::sdkgen::limits_max();
    format!(
        "/* {module}: {NOTE}. Contract maxima of sdk/sdk.toml [limits.max]. */\n\
         #define DL_AREA_MAX_BYTES {:#x}u /* [limits.max] dsp_area_bytes: the largest area the 17-bit word offset of a chunk reaches */\n\
         #define DL_AREA_MAX_WORDS {:#x}u /* the same in words */\n",
        max.dsp_area_bytes,
        max.dsp_area_bytes / 4
    )
}

fn gas_shared(module: &str, per_dev: &[(String, Vec<C>)]) -> Result<String> {
    let mut s = format!("/* {module}: {NOTE}. Constants that every device file agrees on. */\n");
    for k in shared(module, per_dev)? {
        let _ = writeln!(
            s,
            "    .equ {}, {}    /* {} */",
            k.name,
            hex(k.value),
            k.note
        );
    }
    Ok(s)
}

fn gas(module: &str, dev: &str, cs: &[C]) -> String {
    let mut s = format!("/* {module} on {dev}: {NOTE}. */\n");
    for k in cs {
        let _ = writeln!(
            s,
            "    .equ {}, {}    /* {} */",
            k.name,
            hex(k.value),
            k.note
        );
    }
    s
}

fn sharc(module: &str, dev: &str, cs: &[C]) -> String {
    let mut s = format!(
        "// {module} on {dev}: {NOTE}.\n// Decimal values: the assembler upper-cases macro text, which breaks `0x` literals after `+`.\n"
    );
    for k in cs {
        // the note goes on its own line: the assembler's preprocessor keeps a trailing comment in the macro body, where it would
        // swallow the rest of a line that uses the macro (`R3 = DL_TAIL_WORDS - 3;`)
        let _ = writeln!(s, "// {}\n#define {} {}", k.note, k.name, k.value);
    }
    s
}

/// The files a module asks for with `[[gen]]` (empty: it asks for none).
pub fn generate(lib: &Library, facts: &Facts, module: &str) -> Result<Vec<GenFile>> {
    let m = lib
        .get(module)
        .ok_or_else(|| err!("unknown module {module}"))?;
    if m.manifest.gens.is_empty() {
        return Ok(vec![]);
    }
    if facts.docs.is_empty() {
        return Err(err!(
            "{module}: [[gen]] needs device files with [board], [digikit] and [card.store]"
        ));
    }
    let mut per_dev: Vec<(String, Vec<C>)> = vec![];
    for (dev, e) in &facts.docs {
        per_dev.push((dev.clone(), constants(lib, dev, e)?));
    }
    let mut out = vec![];
    for g in &m.manifest.gens {
        match g.kind.as_str() {
            "rust" => {
                if g.path.contains("{dev}") {
                    return Err(err!("{module}: a `rust` gen is one file for all devices"));
                }
                out.push((g.path.clone(), rust(module, &per_dev)?));
            }
            "c-shared" => {
                if g.path.contains("{dev}") {
                    return Err(err!(
                        "{module}: a `c-shared` gen is one file for all devices"
                    ));
                }
                out.push((g.path.clone(), c_shared(module)));
            }
            "gas-shared" => {
                if g.path.contains("{dev}") {
                    return Err(err!(
                        "{module}: a `gas-shared` gen is one file for all devices"
                    ));
                }
                out.push((g.path.clone(), gas_shared(module, &per_dev)?));
            }
            "gas" | "sharc" => {
                if !g.path.contains("{dev}") {
                    return Err(err!(
                        "{module}: a `{}` gen is one file per device: put {{dev}} in the path",
                        g.kind
                    ));
                }
                for (dev, cs) in &per_dev {
                    let text = if g.kind == "gas" {
                        gas(module, dev, cs)
                    } else {
                        sharc(module, dev, cs)
                    };
                    out.push((g.path.replace("{dev}", dev), text));
                }
            }
            "sharc-adapter" => {
                if !g.path.contains("{dev}") {
                    return Err(err!(
                        "{module}: a `sharc-adapter` gen is one file per device: put {{dev}} in the path"
                    ));
                }
                for (dev, e) in &facts.docs {
                    let a = e
                        .adapter
                        .as_ref()
                        .ok_or_else(|| err!("{module}: {dev} has no [adapter.machine]"))?;
                    let mut constants = adapter_constants(a);
                    if let Some(p) = e.sample_pool.as_ref().filter(|p| p.ring_r_base != 0) {
                        constants.push(c(
                            "ADM_RING_R_BASE",
                            p.ring_r_base,
                            Scope::PerDevice,
                            "[adapter.sample_pool] ring_r_base (byte address of the 16 R rings of 32 KiB, one per slot)",
                        ));
                    }
                    out.push((g.path.replace("{dev}", dev), sharc(module, dev, &constants)));
                }
            }
            "gas-sample-pool" => {
                if !g.path.contains("{dev}") {
                    return Err(err!(
                        "{module}: a `gas-sample-pool` gen is one file per device: put {{dev}} in the path"
                    ));
                }
                for (dev, e) in &facts.docs {
                    let p = e
                        .sample_pool
                        .as_ref()
                        .ok_or_else(|| err!("{module}: {dev} has no [adapter.sample_pool]"))?;
                    out.push((
                        g.path.replace("{dev}", dev),
                        gas(module, dev, &sample_pool_constants(p)),
                    ));
                }
            }
            "sharc-sample-pool" => {
                if !g.path.contains("{dev}") {
                    return Err(err!(
                        "{module}: a `sharc-sample-pool` gen is one file per device: put {{dev}} in the path"
                    ));
                }
                for (dev, e) in &facts.docs {
                    // a device that borrows the stock pool has no range and no DSP receiver
                    let Some(p) = e.sample_pool.as_ref().filter(|p| p.ddr_bytes != 0) else {
                        continue;
                    };
                    out.push((
                        g.path.replace("{dev}", dev),
                        sharc(module, dev, &sample_pool_dsp_constants(p)),
                    ));
                }
            }
            k => return Err(err!("{module}: unknown gen kind {k:?}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod sample_pool_tests {
    use super::*;

    fn pool(base: u64, bytes: u64) -> Option<AdapterSamplePool> {
        Some(AdapterSamplePool {
            slot_floor: 0,
            ddr_base: base,
            ddr_bytes: bytes,
            ring_r_base: 0,
        })
    }

    #[test]
    fn range_must_clear_the_stock_writes_and_stay_in_the_alias() {
        assert!(sample_pool_facts("dn2", pool(0x8053_1000, 0x1000_0000)).is_ok());
        assert!(sample_pool_facts("dn2", pool(0x9ff0_0000, 0x10_0000)).is_ok());
        // below the last stock write seen (finding 106)
        assert!(sample_pool_facts("dn2", pool(0x8053_0000, 0x1000)).is_err());
        // past the end of the NW32 alias
        assert!(sample_pool_facts("dn2", pool(0x9ff0_0000, 0x20_0000)).is_err());
        // no range at all is a device that borrows the stock pool
        assert!(sample_pool_facts("dt2", pool(0, 0)).is_ok());
    }

    #[test]
    fn the_r_rings_sit_outside_the_pool_and_inside_the_alias() {
        let rings = |base: u64, ring: u64| {
            Some(AdapterSamplePool {
                slot_floor: 0,
                ddr_base: base,
                ddr_bytes: 0x0ff8_0000,
                ring_r_base: ring,
            })
        };
        // the top of the pool range, as in devices/digitone-ii.toml
        assert!(sample_pool_facts("dn2", rings(0x8053_1000, 0x904b_1000)).is_ok());
        // inside the pool
        assert!(sample_pool_facts("dn2", rings(0x8053_1000, 0x9000_0000)).is_err());
        // half inside the pool
        assert!(sample_pool_facts("dn2", rings(0x8053_1000, 0x9049_1000)).is_err());
        // below the last stock write, unaligned, past the alias
        assert!(sample_pool_facts("dn2", rings(0x8053_1000, 0x8053_0000)).is_err());
        assert!(sample_pool_facts("dn2", rings(0x8053_1000, 0x904b_1800)).is_err());
        assert!(sample_pool_facts("dn2", rings(0x8053_1000, 0x9ff8_1000)).is_err());
        // none
        assert!(sample_pool_facts("dn2", rings(0x8053_1000, 0)).is_ok());
    }

    #[test]
    fn dsp_constants_are_the_nw32_view_of_the_range() {
        let p = SamplePoolF {
            slot_floor: 0,
            ddr_base: 0x8053_1000,
            ddr_bytes: 0x1000_0000,
            ring_r_base: 0,
        };
        let c = sample_pool_dsp_constants(&p);
        assert_eq!((c[0].name, c[0].value), ("SP_DDR_BASE_NW32", 0x1014_c400));
        assert_eq!((c[1].name, c[1].value), ("SP_DDR_PAGES", 65536));
    }
}
