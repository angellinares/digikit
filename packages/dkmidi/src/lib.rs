//! A real MIDI port for the digikit SysEx protocol. The workbench (Tauri, `apps/desktop/src-tauri/src/midi_port.rs`) and the `dkmidi` bridge
//! (`src/main.rs`, SysEx on stdin and stdout) share this code. The protocol itself is spoken in TypeScript (`packages/dk-session`); this
//! crate only moves whole SysEx messages: it lists the ports, opens one by name (input and output of the same name), sends a message and
//! hands over the received ones. Nothing here knows a device: the port is whatever the caller picks.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc::{Receiver, RecvTimeoutError, Sender, channel},
};
use std::time::{Duration, Instant};

use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

/// A device's SysEx can reach the callback in pieces (CoreMIDI splits a long message); this puts the pieces together. Bytes outside a SysEx
/// message (clock, active sensing, notes) are dropped. A message that starts inside another one drops the unfinished one.
#[derive(Default)]
pub struct Sysex {
    open: Vec<u8>,
}

impl Sysex {
    /// Feed the bytes of one callback; returns the messages (`F0 ... F7`) it completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut done = Vec::new();
        for &b in bytes {
            if b == 0xF0 {
                self.open.clear();
                self.open.push(b);
            } else if self.open.is_empty() {
                continue;
            } else if b == 0xF7 {
                self.open.push(b);
                done.push(std::mem::take(&mut self.open));
            } else if b < 0x80 {
                self.open.push(b);
            } else if b < 0xF8 {
                // a status byte inside a SysEx ends it without F7: drop it
                self.open.clear();
            }
            // real-time bytes (0xF8 and up) may sit anywhere: ignored
        }
        done
    }
}

/// The names that are both an input and an output port, in the input's order.
pub fn common_ports(inputs: &[String], outputs: &[String]) -> Vec<String> {
    inputs
        .iter()
        .filter(|n| outputs.contains(n))
        .cloned()
        .collect()
}

pub struct Open {
    pub out: MidiOutputConnection,
    /// Keeps the input connection alive; dropping it closes the port.
    _input: MidiInputConnection<()>,
    pub rx: Arc<Mutex<Receiver<Vec<u8>>>>,
}

impl Open {
    /// Send one whole message.
    pub fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.out.send(bytes).map_err(|e| e.to_string())
    }
}

/// How a wait for the next message ended.
#[derive(Debug, PartialEq)]
pub enum Recv {
    Message(Vec<u8>),
    Timeout,
    Cancelled,
    Closed,
}

/// How often a waiting recv looks at the cancel counter.
const CANCEL_SLICE: Duration = Duration::from_millis(40);

/// Wait up to `timeout` for the next message, in short slices so a change of `cancel` (from the value `seen`) ends the wait at once.
/// Blocks the calling thread: run it on a worker.
pub fn recv_message(
    rx: &Receiver<Vec<u8>>,
    timeout: Duration,
    cancel: &AtomicU64,
    seen: u64,
) -> Recv {
    let end = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Acquire) != seen {
            return Recv::Cancelled;
        }
        let left = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(CANCEL_SLICE)) {
            Ok(m) => return Recv::Message(m),
            Err(RecvTimeoutError::Disconnected) => return Recv::Closed,
            Err(RecvTimeoutError::Timeout) if left <= CANCEL_SLICE => return Recv::Timeout,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn names_in(input: &MidiInput) -> Vec<String> {
    input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .collect()
}

fn names_out(output: &MidiOutput) -> Vec<String> {
    output
        .ports()
        .iter()
        .filter_map(|p| output.port_name(p).ok())
        .collect()
}

/// The MIDI ports a device could be on: names that exist as input and as output. `client` names the MIDI client.
pub fn ports(client: &str) -> Result<Vec<String>, String> {
    let input = MidiInput::new(client).map_err(|e| e.to_string())?;
    let output = MidiOutput::new(client).map_err(|e| e.to_string())?;
    Ok(common_ports(&names_in(&input), &names_out(&output)))
}

/// Open the port called `name` (exact match, as input and as output). `client` names the MIDI client and its connections.
pub fn open(client: &str, name: &str) -> Result<Open, String> {
    let mut input = MidiInput::new(client).map_err(|e| e.to_string())?;
    input.ignore(Ignore::None); // SysEx is what this is for
    let output = MidiOutput::new(client).map_err(|e| e.to_string())?;
    let in_port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).is_ok_and(|n| n == name))
        .ok_or_else(|| format!("no MIDI input port {name:?}"))?;
    let out_port = output
        .ports()
        .into_iter()
        .find(|p| output.port_name(p).is_ok_and(|n| n == name))
        .ok_or_else(|| format!("no MIDI output port {name:?}"))?;
    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = channel();
    let mut assembler = Sysex::default();
    let conn = input
        .connect(
            &in_port,
            &format!("{client} in"),
            move |_, bytes, _| {
                for m in assembler.feed(bytes) {
                    let _ = tx.send(m);
                }
            },
            (),
        )
        .map_err(|e| e.to_string())?;
    let out = output
        .connect(&out_port, &format!("{client} out"))
        .map_err(|e| e.to_string())?;
    Ok(Open {
        out,
        _input: conn,
        rx: Arc::new(Mutex::new(rx)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_in_pieces_is_put_together_and_realtime_bytes_are_ignored() {
        let mut s = Sysex::default();
        assert!(s.feed(&[0xFE, 0xF8]).is_empty());
        assert!(s.feed(&[0xF0, 0x7D, 0x64]).is_empty());
        assert!(s.feed(&[0xF8, 0x6B, 0x01]).is_empty());
        assert_eq!(
            s.feed(&[0x02, 0xF7, 0xFE]),
            vec![vec![0xF0, 0x7D, 0x64, 0x6B, 0x01, 0x02, 0xF7]]
        );
        // two messages in one callback
        assert_eq!(
            s.feed(&[0xF0, 1, 0xF7, 0xF0, 2, 0xF7]),
            vec![vec![0xF0, 1, 0xF7], vec![0xF0, 2, 0xF7]]
        );
    }

    #[test]
    fn an_unfinished_message_is_dropped_when_another_starts_or_a_status_byte_comes() {
        let mut s = Sysex::default();
        assert_eq!(
            s.feed(&[0xF0, 1, 2, 0xF0, 3, 0xF7]),
            vec![vec![0xF0, 3, 0xF7]]
        );
        assert!(s.feed(&[0xF0, 1, 0x90, 2, 0xF7]).is_empty());
        assert_eq!(s.feed(&[0xF0, 9, 0xF7]), vec![vec![0xF0, 9, 0xF7]]);
    }

    /// A recv with no device behind it ends at its timeout, on a worker thread, and the thread that waits for it is free meanwhile.
    #[test]
    fn a_recv_with_no_device_returns_at_the_timeout_on_a_worker() {
        let (_tx, rx) = channel::<Vec<u8>>(); // the sender stays alive and silent: nothing answers
        let cancel = Arc::new(AtomicU64::new(0));
        let started = Instant::now();
        let worker = {
            let cancel = cancel.clone();
            std::thread::spawn(move || recv_message(&rx, Duration::from_millis(300), &cancel, 0))
        };
        // the caller is not blocked: it can do other work while the worker waits
        assert!(!worker.is_finished());
        assert_eq!(worker.join().unwrap(), Recv::Timeout);
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(290) && took < Duration::from_millis(1500),
            "{took:?}"
        );
    }

    #[test]
    fn a_cancel_ends_a_long_wait_at_once_and_a_message_still_arrives() {
        let (tx, rx) = channel::<Vec<u8>>();
        let cancel = Arc::new(AtomicU64::new(0));
        let worker = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                (
                    recv_message(&rx, Duration::from_secs(30), &cancel, 0),
                    started.elapsed(),
                    rx,
                )
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        cancel.fetch_add(1, Ordering::AcqRel);
        let (got, took, rx) = worker.join().unwrap();
        assert_eq!(got, Recv::Cancelled);
        assert!(took < Duration::from_secs(2), "{took:?}");
        // a later wait (it sees the new counter) is not cancelled by the old cancel
        tx.send(vec![0xF0, 1, 0xF7]).unwrap();
        assert_eq!(
            recv_message(&rx, Duration::from_secs(1), &cancel, 1),
            Recv::Message(vec![0xF0, 1, 0xF7])
        );
        drop(tx);
        assert_eq!(
            recv_message(&rx, Duration::from_secs(1), &cancel, 1),
            Recv::Closed
        );
    }

    #[test]
    fn only_names_that_are_both_input_and_output_are_offered() {
        let i = ["A".to_string(), "B".to_string(), "C".to_string()];
        let o = ["C".to_string(), "A".to_string()];
        assert_eq!(common_ports(&i, &o), vec!["A".to_string(), "C".to_string()]);
    }
}
