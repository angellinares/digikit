//! The image table `machines_info` and the assembler that reads it: `image.rs` is compiled into this test by path (the crate's own `lib.rs` pulls in the assembler, which a host
//! cannot assemble). The table is bound over a `FakeCore` (the core's info entry), refused when it is not this device's or lacks the registry, and the byte offsets the
//! assembler files name (`.equ T_*`) are the word indexes of `image::w`.
use dkrt_svc_rt::name_hash;
use dkrt_test_support::FakeCore;

#[path = "../src/image.rs"]
mod image;
use image::*;

fn table(device: u32) -> Vec<u32> {
    let mut t = vec![0u32; WORDS];
    t[w::MAGIC] = MAGIC;
    t[w::VERSION] = VERSION;
    t[w::DEVICE] = device;
    t[w::ENSURE] = 0x4000_1000;
    t[w::ROWS] = 0x4200_0000;
    t[w::FIRST] = 7;
    t[w::COUNT] = 3;
    t[w::R_FT] = 0x4002_7582;
    t
}

fn core_with(words: Option<Vec<u32>>, device: u32) -> FakeCore {
    let mut c = FakeCore::new();
    c.device = device;
    if let Some(t) = words {
        c.info_tables.push((name_hash("machines_info"), t));
    }
    c
}

#[test]
fn the_table_is_copied_with_the_magic_last_and_every_word_in() {
    let core = core_with(Some(table(2)), 2);
    let mut dst = [0xffff_ffffu32; WORDS];
    assert_eq!(unsafe { bind_table(&core, dst.as_mut_ptr()) }, 0);
    assert_eq!(dst[..], table(2)[..]);
}

#[test]
fn an_image_without_the_table_another_device_or_no_registry_is_refused() {
    let mut dst = [0u32; WORDS];
    let none = core_with(None, 2);
    assert_eq!(
        unsafe { bind_table(&none, dst.as_mut_ptr()) },
        dkrt_sdk::E_NO_REGISTRY
    );
    let other = core_with(Some(table(1)), 2);
    assert_eq!(
        unsafe { bind_table(&other, dst.as_mut_ptr()) },
        dkrt_sdk::E_NO_REGISTRY
    );
    let mut bare = table(2);
    bare[w::ROWS] = 0;
    assert_eq!(
        unsafe { bind_table(&core_with(Some(bare), 2), dst.as_mut_ptr()) },
        dkrt_sdk::E_NO_REGISTRY
    );
    let mut old = table(2);
    old[w::VERSION] = VERSION + 1;
    assert_eq!(
        unsafe { bind_table(&core_with(Some(old), 2), dst.as_mut_ptr()) },
        dkrt_sdk::E_NO_REGISTRY
    );
    assert_eq!(dst, [0u32; WORDS], "nothing is written for a refused table");
}

/// `.equ T_NAME, value` lines of an assembler file.
fn equates(text: &str) -> Vec<(String, usize)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix(".equ")?.trim();
            let (name, value) = rest.split_once(',')?;
            let name = name.trim();
            name.starts_with("T_").then(|| {
                (
                    name.to_string(),
                    value.split('|').next().unwrap().trim().parse().unwrap(),
                )
            })
        })
        .collect()
}

#[test]
fn the_offsets_the_assembler_names_are_the_word_indexes_of_the_table() {
    for file in [
        include_str!("../src/hooks-common.s"),
        include_str!("../src/hooks-dt2.s"),
        include_str!("../src/hooks-dn2.s"),
    ] {
        let eq = equates(file);
        assert!(!eq.is_empty());
        for (name, at) in eq {
            let index = ASM_OFFSETS
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("{name} is not in ASM_OFFSETS"))
                .1;
            assert_eq!(at, 4 * index, "{name}");
        }
    }
}

#[test]
fn the_tail_a_hook_names_is_the_hash_of_its_hook() {
    // `dk_tail_<8 hex digits of name_hash(hook)>` (patchkit `bind::tail_import`) in the assembler: every one is the tail of a hook of the service
    let hooks = [
        "cf.machine.hook.type_page.count",
        "cf.machine.hook.type_page.desc",
        "cf.machine.hook.param_code.map",
        "cf.machine.hook.param_code.args",
        "cf.page.hook.source_draw",
        "cf.page.hook.source_code",
        "cf.page.hook.source_key",
        "cf.page.hook.source_turn",
    ];
    let all = [
        include_str!("../src/hooks-common.s"),
        include_str!("../src/hooks-dt2.s"),
        include_str!("../src/hooks-dn2.s"),
    ]
    .concat();
    for h in hooks {
        let tail = format!("dk_tail_{:08x}", name_hash(h));
        assert!(all.contains(&tail), "{h}: the assembler has no {tail}");
    }
    for (i, _) in all.match_indices("dk_tail_") {
        let t = &all[i..i + 16];
        if !t[8..].bytes().all(|c| c.is_ascii_hexdigit()) {
            continue; // prose: `dk_tail_<hash of the hook>`
        }
        assert!(
            hooks
                .iter()
                .any(|h| t == format!("dk_tail_{:08x}", name_hash(h))),
            "{t} is the tail of no hook"
        );
    }
}
