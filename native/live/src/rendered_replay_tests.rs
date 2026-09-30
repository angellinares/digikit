//! Bounded native-SHARC diagnostic for an operator-checked local DTFR and
//! matching state pack. The extra repeats are a *synthetic* queue schedule;
//! this does not reproduce GUI/audio callback scheduling or Device behavior.

use std::{env, ffi::CString, path::Path};

use crate::{
    FRAME_LEN,
    abi::{
        LiveRenderStats, live_close, live_open_frames_with_rendered_input_log, live_push_frame,
        live_render, live_render_stats,
    },
    sharc_source::LivePack,
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
    let lib = input("SHARC_RENDER_LIB");
    let frames = frames(Path::new(&expected));
    let max: usize = env::var("SHARC_RENDER_FRAMES").unwrap().parse().unwrap();
    assert!((1..=256).contains(&max) && max == frames.len());
    let max_renders = max + max / 3;
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
    let lib = CString::new(lib).unwrap();
    let pack_path = CString::new(input("SHARC_RENDER_PACK")).unwrap();
    let card = CString::new(env::var("SHARC_RENDER_CARD_SHA256").unwrap()).unwrap();
    let path_c = CString::new(path.clone()).unwrap();
    // Exercise the exported offline ABI and its production player/queue
    // consumer, including per-frame artifact flushing after a stop.
    let handle = unsafe {
        live_open_frames_with_rendered_input_log(
            0,
            lib.as_ptr(),
            pack_path.as_ptr(),
            card.as_ptr(),
            1.0,
            max_renders as u32,
            path_c.as_ptr(),
        )
    };
    assert!(!handle.is_null());
    let mut out = vec![0.0f32; FRAME_LEN * 2];
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            unsafe { live_push_frame(handle, frame.as_ptr(), frame.len()) },
            0
        );
        assert_eq!(
            unsafe { live_render(handle, 1, out.as_mut_ptr(), out.len()) },
            1
        );
        if (index + 1) % 3 == 0 {
            assert_eq!(
                unsafe { live_render(handle, 1, out.as_mut_ptr(), out.len()) },
                1
            );
        }
    }
    let mut log = LiveRenderStats::default();
    assert_eq!(unsafe { live_render_stats(handle, &mut log) }, 0);
    unsafe { live_close(handle) };
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), max_renders);
    assert!(
        text.lines()
            .all(|line| line.contains("\"frame_end\":\"clean\""))
    );
    println!(
        "rendered: {} taken + {} repeats = {} total; {} native SHARC instructions; stopped {}",
        max,
        max / 3,
        log.frames,
        log.instructions,
        log.stopped
    );
    assert_eq!(log.frames, max_renders as u64);
    assert_eq!(
        log.stopped, 0,
        "zero-stopped-render gate failed; input log retained in ignored out/"
    );
}
