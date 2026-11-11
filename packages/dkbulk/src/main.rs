//! `dkbulk`: the USB bulk link of the digikit usb-fast module, for the command-line tools.
//!
//!     dkbulk list                          the devices that offer a bulk interface: ID (bus:address), name, interface number
//!     dkbulk open [ID]                     bridge: whole request frames on stdin go to the device, every reply frame comes out on stdout
//!                                          (the TypeScript session code of tools/devtools/wb.mjs speaks the protocol over it)
//!     dkbulk echo [--id ID] [--sizes N,N,...]
//!                                          the first hardware run (test/usb-echo, step 3): send each size of bytes, read the same bytes back, compare, print MB/s
//!     dkbulk put [--id ID] [--root N] [--chunk N] [--trailer] [--replace] LOCAL REMOTE [LOCAL REMOTE ...]
//!                                          upload files with the Rust client and print MB/s (--trailer: the expected hash covers 16 zero bytes more, as on the DT2 +Drive)
use std::io::{Read, Write};
use std::process::exit;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dkbulk::link::InPipe;
use dkbulk::usb::{self, UsbLink};
use dkbulk::{Client, Options, gen_bulk, proto};

fn die(code: i32, msg: &str) -> ! {
    eprintln!("dkbulk: {msg}");
    exit(code);
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if args.is_empty() {
        String::new()
    } else {
        args.remove(0)
    };
    match cmd.as_str() {
        "list" => match usb::list() {
            Ok(v) => v
                .iter()
                .for_each(|c| println!("{}\t{}\tinterface {}", c.id, c.name, c.interface)),
            Err(e) => die(1, &e),
        },
        "open" => bridge(args.first().map(String::as_str)),
        "put" => put(args),
        "echo" => echo(args),
        _ => die(
            2,
            "usage: dkbulk list | dkbulk open [ID] | dkbulk echo [--id ID] [--sizes N,N,...] | dkbulk put [--id ID] [--root N] [--chunk N] [--trailer] [--replace] LOCAL REMOTE ...",
        ),
    }
}

fn bridge(id: Option<&str>) {
    let link = UsbLink::open(id).unwrap_or_else(|e| die(1, &e));
    eprintln!("open");
    let (mut tx, mut rx, _interface) = link.split();
    std::thread::spawn(move || {
        let (mut stdin, mut buf) = (std::io::stdin().lock(), Vec::<u8>::new());
        let mut chunk = vec![0u8; 1 << 16];
        loop {
            match stdin.read(&mut chunk) {
                Ok(0) => exit(0),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => die(1, &format!("stdin: {e}")),
            }
            while let Ok(total) = proto::frame_len(&buf) {
                if buf.len() < total {
                    break;
                }
                let frame: Vec<u8> = buf.drain(..total).collect();
                if let Err(e) = tx.send(&frame, Duration::from_secs(10)) {
                    die(1, &e.to_string());
                }
            }
            if buf.len() >= gen_bulk::HEADER && proto::frame_len(&buf).is_err() {
                die(1, "stdin: not a bulk frame");
            }
        }
    });
    let mut out = std::io::stdout().lock();
    loop {
        match rx.recv(Duration::from_secs(3600)) {
            Ok(f) => {
                if out.write_all(&f).and_then(|_| out.flush()).is_err() {
                    exit(0);
                }
            }
            Err(dkbulk::LinkError::Timeout) => {}
            Err(e) => die(1, &e.to_string()),
        }
    }
}

fn put(mut args: Vec<String>) {
    let mut take = |flag: &str| -> Option<String> {
        let at = args.iter().position(|a| a == flag)?;
        args.remove(at);
        (at < args.len()).then(|| args.remove(at))
    };
    let device = take("--id");
    let root: u32 = take("--root").map_or(1, |v| v.parse().unwrap_or_else(|_| die(2, "--root N")));
    let chunk: usize = take("--chunk").map_or(gen_bulk::DATA_MAX, |v| {
        v.parse().unwrap_or_else(|_| die(2, "--chunk N"))
    });
    let replace = args
        .iter()
        .position(|a| a == "--replace")
        .map(|i| args.remove(i))
        .is_some();
    let trailer = args
        .iter()
        .position(|a| a == "--trailer")
        .map(|i| args.remove(i))
        .is_some();
    if args.is_empty() || !args.len().is_multiple_of(2) {
        die(2, "put wants LOCAL REMOTE pairs");
    }
    let link = UsbLink::open(device.as_deref()).unwrap_or_else(|e| die(1, &e));
    let mut client = Client::new(
        link,
        Options {
            chunk,
            ..Options::default()
        },
    );
    match client.info() {
        Ok(i) => eprintln!(
            "device: bulk version {}, data_max {}, align {}",
            i.version, i.data_max, i.align
        ),
        Err(e) => die(1, &format!("INFO: {e}")),
    }
    let cancel = AtomicBool::new(false);
    let (mut bytes, start) = (0usize, Instant::now());
    for pair in args.chunks(2) {
        let data = std::fs::read(&pair[0]).unwrap_or_else(|e| die(1, &format!("{}: {e}", pair[0])));
        let mut hashed = data.clone();
        if trailer {
            hashed.extend_from_slice(&[0; 16]);
        }
        let expected = dksample::hash::content_hash(&hashed) | 1;
        let t = Instant::now();
        match client.put(
            root,
            &pair[1],
            &data,
            Some(expected),
            replace,
            &cancel,
            &mut |_, _| {},
        ) {
            Ok(h) => {
                let s = t.elapsed().as_secs_f64();
                println!(
                    "{}\t{} bytes\t{:.3} s\t{:.2} MB/s\thash {h:08x}",
                    pair[1],
                    data.len(),
                    s,
                    data.len() as f64 / 1e6 / s
                );
                bytes += data.len();
            }
            Err(e) => die(1, &format!("{}: {e}", pair[1])),
        }
    }
    let s = start.elapsed().as_secs_f64();
    println!(
        "total {bytes} bytes {:.3} s {:.2} MB/s",
        s,
        bytes as f64 / 1e6 / s
    );
}

/// The sizes of the first hardware run (docs/plans/usb-fast-hardware.md, DN2 run 1): around the 512-byte packet, a 16 KiB buffer, a frame and 1 MiB.
const ECHO_SIZES: &[usize] = &[1, 511, 512, 513, 4096, 16384, 65536, 1_048_576];

/// Send `size` bytes of a pattern to the echo module of `test/usb-echo` and read them back: the bytes must be identical. Raw bytes, not frames.
fn echo(mut args: Vec<String>) {
    let mut take = |flag: &str| -> Option<String> {
        let at = args.iter().position(|a| a == flag)?;
        args.remove(at);
        (at < args.len()).then(|| args.remove(at))
    };
    let device = take("--id");
    let sizes: Vec<usize> = take("--sizes").map_or_else(
        || ECHO_SIZES.to_vec(),
        |v| {
            v.split(',')
                .map(|n| {
                    n.trim()
                        .parse()
                        .unwrap_or_else(|_| die(2, "--sizes N,N,..."))
                })
                .collect()
        },
    );
    let mut link = UsbLink::open(device.as_deref()).unwrap_or_else(|e| die(1, &e));
    let mut failed = false;
    for (i, &size) in sizes.iter().enumerate() {
        let data: Vec<u8> = (0..size)
            .map(|k| (k.wrapping_mul(7) + (k >> 8) + i) as u8)
            .collect();
        let start = Instant::now();
        // The reads run beside the send, because the device holds only a few buffers and waits for the host to take its replies.
        let back = std::thread::scope(|scope| {
            let tx = &mut link.tx;
            let data = &data;
            let sender = scope.spawn(move || tx.send(data, Duration::from_secs(10)));
            let mut back = Vec::with_capacity(size);
            while back.len() < size {
                match link.rx.read(Duration::from_secs(10)) {
                    Ok(b) => back.extend_from_slice(&b),
                    Err(e) => die(
                        1,
                        &format!("read after {} of {size} bytes: {e}", back.len()),
                    ),
                }
            }
            if let Err(e) = sender.join().expect("sender thread") {
                die(1, &format!("send of {size} bytes: {e}"));
            }
            back
        });
        let s = start.elapsed().as_secs_f64();
        let same = back == data;
        failed |= !same;
        println!(
            "{size}\t{}\t{:.3} s\t{:.2} MB/s (both ways)",
            if same { "identical" } else { "DIFFERENT" },
            s,
            2.0 * size as f64 / 1e6 / s
        );
    }
    if failed {
        exit(1);
    }
}
