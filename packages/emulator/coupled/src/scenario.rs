//! Scripted inputs for coupled runs (tests/scenarios/dn2.json), shared by coupled-run and the desktop app's --scenario mode.
use serde_json::Value;

/// DSPI2 frames per audio second (one 32-sample block each).
pub const FRAMES_PER_SECOND: f64 = 1500.0;
/// ColdFire instructions per emulated second: 132,000,000 (one per bus tick, so 88,000 per DSPI2
/// frame at 1500 Hz) unless `DIGI_CF_IPS` says otherwise. Instruction counts in scenario files
/// are authored at 132,000,000 and are scaled to this rate when parsed.
pub fn cf_instructions_per_second() -> f64 {
    elektron_native_boot::clock::cf_ips() as f64
}
pub const DEFAULT_SCENARIOS: &str = include_str!("../../../../tests/scenarios/dn2.json");

#[derive(Clone, Copy, Debug)]
pub enum When {
    Instr(u64),
    Frames(u64),
}

/// Most bytes in one `midi` event (an event stays `Copy`).
pub const MIDI_EVENT_MAX: usize = 16;

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Button {
        code: u8,
        down: bool,
    },
    Turn {
        encoder: u8,
        detents: i32,
    },
    /// Bytes on the MIDI DIN input, one byte per wire time from the event.
    Midi {
        len: u8,
        bytes: [u8; MIDI_EVENT_MAX],
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub when: When,
    /// The `at_s` the event was written with (zero DSP turns it into instructions).
    pub at_s: Option<f64>,
    pub action: Action,
}

#[derive(Debug)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    /// End after this many audio seconds, or this many ColdFire instructions.
    pub seconds: Option<f64>,
    pub instructions: Option<u64>,
    pub qa_script: bool,
    /// With `instructions`: shorten the last chunk so the run ends exactly there
    /// (the desktop Trig 1 test does); otherwise chunks are always full, as in the app.
    pub clip_last_chunk: bool,
    /// Shorten chunks so each `at_instr` event fires at exactly that count.
    pub exact_events: bool,
    /// SHA-256 the run's whole PCM must have (a recorded reference).
    pub expect_sha256: Option<String>,
    pub events: Vec<Event>,
}

pub fn parse_scenarios(text: &str) -> Result<Vec<Scenario>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("scenario file: {e}"))?;
    let list = root["scenarios"]
        .as_array()
        .ok_or("scenario file needs a \"scenarios\" array")?;
    list.iter().map(parse_scenario).collect()
}

pub fn parse_scenario(v: &Value) -> Result<Scenario, String> {
    let name = v["name"]
        .as_str()
        .ok_or("scenario without a name")?
        .to_string();
    let err = |m: &str| format!("scenario {name}: {m}");
    let seconds = v["seconds"].as_f64();
    let ips = elektron_native_boot::clock::cf_ips();
    let instructions = v["instructions"]
        .as_u64()
        .map(|n| elektron_native_boot::clock::scale_instr(n, ips));
    if seconds.is_none() == instructions.is_none() {
        return Err(err("give exactly one of \"seconds\" and \"instructions\""));
    }
    let mut events = Vec::new();
    for e in v["events"].as_array().map_or(&[][..], Vec::as_slice) {
        let when = match (e["at_s"].as_f64(), e["at_instr"].as_u64()) {
            (Some(s), None) if s >= 0.0 => When::Frames((s * FRAMES_PER_SECOND).round() as u64),
            (None, Some(n)) => When::Instr(elektron_native_boot::clock::scale_instr(n, ips)),
            _ => return Err(err("an event needs one of at_s (>= 0) and at_instr")),
        };
        let action = if let Some(code) = e["button"].as_u64() {
            Action::Button {
                code: u8::try_from(code).map_err(|_| err("button code over 255"))?,
                down: e["down"].as_bool().unwrap_or(true),
            }
        } else if let Some(enc) = e["turn"].as_u64() {
            Action::Turn {
                encoder: u8::try_from(enc).map_err(|_| err("encoder over 255"))?,
                detents: i32::try_from(e["detents"].as_i64().unwrap_or(1))
                    .map_err(|_| err("detents out of range"))?,
            }
        } else if let Some(list) = e["midi"].as_array() {
            if list.is_empty() || list.len() > MIDI_EVENT_MAX {
                return Err(err("a midi event needs 1 to 16 bytes"));
            }
            let mut bytes = [0u8; MIDI_EVENT_MAX];
            for (slot, b) in bytes.iter_mut().zip(list) {
                *slot = b
                    .as_u64()
                    .and_then(|b| u8::try_from(b).ok())
                    .ok_or_else(|| err("a midi byte must be 0 to 255"))?;
            }
            Action::Midi {
                len: list.len() as u8,
                bytes,
            }
        } else {
            return Err(err("an event needs \"button\", \"turn\" or \"midi\""));
        };
        events.push(Event {
            when,
            at_s: e["at_s"].as_f64(),
            action,
        });
    }
    Ok(Scenario {
        description: v["description"].as_str().unwrap_or("").to_string(),
        name,
        seconds,
        instructions,
        qa_script: v["qa_script"].as_bool().unwrap_or(false),
        clip_last_chunk: v["clip_last_chunk"].as_bool().unwrap_or(false),
        exact_events: v["exact_events"].as_bool().unwrap_or(false),
        expect_sha256: v["expect_sha256"].as_str().map(str::to_string),
        events,
    })
}

pub fn fire(rt: &mut crate::CoupledRuntime, action: Action) -> Result<(), String> {
    match action {
        Action::Button { code, down } => rt.button(code, down),
        Action::Turn { encoder, detents } => rt.turn(encoder, detents),
        Action::Midi { len, bytes } => rt.midi_in(&bytes[..usize::from(len)]),
    }
}

/// The scenario inside `value`: `value` itself when it has an "events" array,
/// else the first "recorded_scenario" found in its objects (a desktop
/// diagnostics export carries one).
pub fn find_scenario(value: &serde_json::Value) -> Option<&serde_json::Value> {
    if value.get("events").is_some_and(serde_json::Value::is_array) {
        return Some(value);
    }
    let object = value.as_object()?;
    if let Some(found) = object.get("recorded_scenario").and_then(find_scenario) {
        return Some(found);
    }
    object.values().find_map(find_scenario)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn find_scenario_reads_a_bare_scenario_and_a_nested_recording() {
        let bare = json!({"events": []});
        assert_eq!(find_scenario(&bare), Some(&bare));
        let nested = json!({"diagnostics": {"recorded_scenario": {"name": "r", "instructions": 5, "events": []}}});
        let found = find_scenario(&nested).unwrap();
        assert_eq!(found["name"], "r");
    }

    #[test]
    fn parse_scenario_reads_midi_events() {
        let sc = parse_scenario(
            &json!({"name":"m","seconds":1,"events":[{"at_instr":4,"midi":[0x99,60,100]}]}),
        )
        .unwrap();
        let Action::Midi { len, bytes } = sc.events[0].action else {
            panic!("not a midi event");
        };
        assert_eq!(&bytes[..usize::from(len)], &[0x99, 60, 100]);
        let bad = |events: serde_json::Value| {
            parse_scenario(&json!({"name":"m","seconds":1,"events":events})).unwrap_err()
        };
        assert!(bad(json!([{"at_instr":0,"midi":[]}])).contains("1 to 16"));
        assert!(bad(json!([{"at_instr":0,"midi":vec![0; 17]}])).contains("1 to 16"));
        assert!(bad(json!([{"at_instr":0,"midi":[256]}])).contains("0 to 255"));
    }

    #[test]
    fn parse_scenario_reads_exact_events() {
        let sc = parse_scenario(&json!({"name":"x","instructions":10,"exact_events":true,"events":[{"at_instr":4,"button":25,"down":true}]})).unwrap();
        assert!(sc.exact_events);
        assert_eq!(sc.events.len(), 1);
    }
}
