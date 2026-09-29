//! Import the Python `components["timers"]` checkpoint component only.
//!
//! This deliberately does not load timer register pages or deliver vectors;
//! those belong to the board/MMIO and CPU application seams respectively.

use crate::state::MachineState;
use periph::machine::Timers;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerStateError {
    Missing,
    Invalid,
    Unsupported,
    Configuration,
}

struct SourceState {
    next: [Option<f64>; 4],
    pending: [bool; 4],
    held: bool,
    fired: [u64; 4],
    missed: [u64; 4],
    cleared: [u64; 4],
    stale: Vec<usize>,
}

/// Import a `Timers` v1 component into an already-configured peripheral lane.
///
/// Saved deadlines are absolute in the Python source's `now` clock. MSTATE's
/// checkpoint clock starts at zero, so each is rebased to `next - now`; this
/// preserves remaining time without inventing an unobserved timer phase.
/// The native lane is always PIT then DTIM, and its configured channel order
/// and instruction rate must exactly match the saved source configuration.
pub fn import_timers(state: &MachineState, timers: &mut Timers) -> Result<(), TimerStateError> {
    let component = state
        .components
        .as_object()
        .and_then(|components| components.get("timers"))
        .ok_or(TimerStateError::Missing)?;
    let root = object(component)?;
    if text(root, "type")? != "Timers" || u32_integer(root, "version")? != 1 {
        return Err(TimerStateError::Unsupported);
    }
    let sources = array(root, "sources")?;
    if sources.len() != 2 {
        return Err(TimerStateError::Configuration);
    }
    let pit = parse_source(
        &sources[0],
        "Pits",
        timers.pit.channels(),
        timers.pit.ips(),
        false,
    )?;
    let dtim = parse_source(
        &sources[1],
        "Dtims",
        timers.dtim.channels(),
        timers.dtim.ips(),
        true,
    )?;

    // All validation precedes these mutations: malformed components leave the
    // existing peripheral lane untouched.
    timers.pit.load_checkpoint_state(
        &pit.next,
        &pit.pending,
        pit.held,
        &pit.fired,
        &pit.missed,
        &pit.cleared,
    );
    timers.dtim.load_checkpoint_state(
        &dtim.next,
        &dtim.pending,
        dtim.held,
        &dtim.fired,
        &dtim.missed,
        &dtim.cleared,
        dtim.stale,
    );
    Ok(())
}

fn parse_source(
    value: &Value,
    expected_type: &str,
    configured_channels: &[usize],
    configured_ips: f64,
    is_dtim: bool,
) -> Result<SourceState, TimerStateError> {
    let source = object(value)?;
    if text(source, "type")? != expected_type || u32_integer(source, "version")? != 1 {
        return Err(TimerStateError::Unsupported);
    }
    let channels = channel_list(source, "channels")?;
    let ips = u32_integer(source, "ips")?;
    if channels != configured_channels || ips as f64 != configured_ips {
        return Err(TimerStateError::Configuration);
    }
    let now = u32_integer(source, "now")? as f64;
    let next_values = array(source, "next")?;
    if next_values.len() != 4 {
        return Err(TimerStateError::Invalid);
    }
    let mut next = [None; 4];
    for (channel, value) in next_values.iter().enumerate() {
        if value.is_null() {
            continue;
        }
        let deadline = value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or(TimerStateError::Invalid)?;
        let relative = deadline - now;
        if !relative.is_finite() || !configured_channels.contains(&channel) {
            return Err(TimerStateError::Invalid);
        }
        next[channel] = Some(relative);
    }
    let pending = optional_channel_flags(source, "pending")?;
    let fired = counters(source, "fired")?;
    let missed = counters(source, "missed")?;
    // `cleared` and `pending` did not exist before held-tick checkpoints.
    let cleared = optional_counters(source, "cleared")?;
    let held = boolean(source, "held")?;
    let stale = if is_dtim {
        let arm = channel_list(source, "arm")?;
        if !arm.is_empty() {
            // ARM_STEP has no native scheduler equivalent, so accepting it
            // would silently lose timing behaviour.
            return Err(TimerStateError::Unsupported);
        }
        channel_list(source, "stale")?
    } else {
        Vec::new()
    };
    Ok(SourceState {
        next,
        pending,
        held,
        fired,
        missed,
        cleared,
        stale,
    })
}

fn object(value: &Value) -> Result<&Map<String, Value>, TimerStateError> {
    value.as_object().ok_or(TimerStateError::Invalid)
}

fn array<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a Vec<Value>, TimerStateError> {
    object
        .get(name)
        .and_then(Value::as_array)
        .ok_or(TimerStateError::Invalid)
}

fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str, TimerStateError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .ok_or(TimerStateError::Invalid)
}

/// Python's v1 checkpoint validator accepts exact u32 integers only.
fn u32_integer(object: &Map<String, Value>, name: &str) -> Result<u32, TimerStateError> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .filter(|number| *number <= u64::from(u32::MAX))
        .map(|number| number as u32)
        .ok_or(TimerStateError::Invalid)
}

fn boolean(object: &Map<String, Value>, name: &str) -> Result<bool, TimerStateError> {
    object
        .get(name)
        .and_then(Value::as_bool)
        .ok_or(TimerStateError::Invalid)
}

fn channel_list(object: &Map<String, Value>, name: &str) -> Result<Vec<usize>, TimerStateError> {
    let mut channels = Vec::new();
    for value in array(object, name)? {
        let channel = value
            .as_u64()
            .filter(|channel| *channel < 4)
            .ok_or(TimerStateError::Invalid)? as usize;
        channels.push(channel);
    }
    Ok(channels)
}

fn optional_channel_flags(
    object: &Map<String, Value>,
    name: &str,
) -> Result<[bool; 4], TimerStateError> {
    if !object.contains_key(name) {
        return Ok([false; 4]);
    }
    let channels = channel_list(object, name)?;
    let mut flags = [false; 4];
    for channel in channels {
        flags[channel] = true;
    }
    Ok(flags)
}

fn optional_counters(object: &Map<String, Value>, name: &str) -> Result<[u64; 4], TimerStateError> {
    if !object.contains_key(name) {
        return Ok([0; 4]);
    }
    counters(object, name)
}

fn counters(object: &Map<String, Value>, name: &str) -> Result<[u64; 4], TimerStateError> {
    let values = object
        .get(name)
        .and_then(Value::as_object)
        .ok_or(TimerStateError::Invalid)?;
    let mut counters = [0; 4];
    for (key, count) in values {
        let channel = key
            .parse::<usize>()
            .ok()
            .filter(|channel| *channel < 4)
            .ok_or(TimerStateError::Invalid)?;
        if channel.to_string() != *key {
            return Err(TimerStateError::Invalid);
        }
        counters[channel] = count
            .as_u64()
            .filter(|count| *count <= u64::from(u32::MAX))
            .ok_or(TimerStateError::Invalid)?;
    }
    Ok(counters)
}
