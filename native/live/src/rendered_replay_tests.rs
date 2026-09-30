//! Bounded native-SHARC diagnostic for an operator-checked local DTFR and
//! matching state pack. The extra repeats are a *synthetic* queue schedule;
//! this does not reproduce GUI/audio callback scheduling or Device behavior.

use std::{env, path::Path};

use crate::{
    FRAME_LEN, StereoSample,
    repeater::FrameQueue,
    sharc_lib::LibCore,
    sharc_source::{LivePack, LiveSource},
    source::FrameSource,
};

fn input(name: &str) -> String {
    let path = env::var(name).unwrap_or_else(|_| panic!("missing {name}"));
    assert!(Path::new(&path).is_absolute(), "{name} must be absolute");
    path
}

fn frames(path: &Path) -> Vec<Vec<u8>> {
    let data = std::fs::read(path).unwrap();
    assert!(data.len() <= 256 * (4 + 4096) + 12 && data.len() >= 12 && &data[..4] == b"DTFR");
    assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()), 1);
    let n = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    assert!((1..=256).contains(&n));
    let mut pos = 12;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        assert!(pos + 4 <= data.len());
        let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        assert!(len <= 4096 && pos + len <= data.len());
        out.push(data[pos..pos + len].to_vec());
        pos += len;
    }
    assert_eq!(pos, data.len());
    out
}

#[test]
#[ignore = "requires checked local SHARC state pack, native core, card hash and ColdFire wire trace"]
fn exact_rendered_input_and_zero_stop_gate() {
    let expected = input("SHARC_RENDER_WIRE");
    let pack = LivePack::load(Path::new(&input("SHARC_RENDER_PACK"))).unwrap();
    pack.check_card(&env::var("SHARC_RENDER_CARD_SHA256").unwrap())
        .unwrap();
    let core = LibCore::open(Path::new(&input("SHARC_RENDER_LIB")), pack.image()).unwrap();
    let mut source = LiveSource::new(core, &pack, 1.0).unwrap();
    let frames = frames(Path::new(&expected));
    let max: usize = env::var("SHARC_RENDER_FRAMES").unwrap().parse().unwrap();
    assert!((1..=256).contains(&max) && max == frames.len());
    let max_renders = max + max / 3;
    let inputs = source.enable_rendered_input_log(max_renders);
    let queue = FrameQueue::new();
    let mut wire = Vec::new();
    let mut out = [StereoSample::default(); FRAME_LEN];
    for (index, frame) in frames.iter().enumerate() {
        queue.write(frame);
        assert!(queue.take_into(&mut wire));
        source.render_frame(&wire, &mut out);
        // Exact post-queue bytes and their SHA-256 also cover a synthetic
        // take/repeat sequence; the repeat clears one-shot release/trig words.
        if (index + 1) % 3 == 0 {
            assert!(queue.take_into(&mut wire));
            source.render_frame(&wire, &mut out);
        }
    }
    let path = input("SHARC_RENDER_INPUTS");
    let ignored = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../out")
        .canonicalize()
        .unwrap();
    assert!(
        Path::new(&path)
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(ignored)
    );
    let inputs = inputs.lock().unwrap();
    inputs.write_ndjson(Path::new(&path)).unwrap();
    assert_eq!(inputs.records().len(), max_renders);
    assert_eq!(inputs.dropped, 0);
    let log = source.log();
    let log = log.lock().unwrap();
    println!(
        "rendered: {} taken + {} synthetic repeats = {} total; {} native SHARC instructions; stopped {}, dma failures {}; first stop {:?}",
        max,
        max / 3,
        log.frames,
        log.instructions,
        log.stopped,
        log.dma_failures,
        log.first_stop
    );
    assert_eq!(
        log.stopped, 0,
        "zero-stopped-render gate failed; input log retained in ignored out/"
    );
    assert_eq!(
        log.dma_failures, 0,
        "SHARC DMA gate failed; input log retained in ignored out/"
    );
}
