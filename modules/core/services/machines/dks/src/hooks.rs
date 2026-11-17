//! Every hook of the machines service as assembler (ColdFire, GNU syntax): `hooks-common.s` (the page entries, one routine for both devices), `hooks-dt2.s` and `hooks-dn2.s`, in one
//! object (a hook is declared for its device only, `module.toml` `devices` and the `service!` call). The files are plain `.s`, included here only, so that the host tests of core/machines
//! assemble them as they are (`tests/test_pk_machines_fw.py`). They read the table `MF_TAB` (`lib.rs`) and call `dks_page_key` and `dks_page_turn` (`fw.rs`).
#[cfg(target_arch = "m68k")]
use core::arch::global_asm;

// The assembler is ColdFire code. A host build (the assembly test of the runtime runs the declarations of `service!`) has an empty function for each entry, so that the entry
// addresses exist.
#[cfg(target_arch = "m68k")]
global_asm!(include_str!("hooks-common.s"));
#[cfg(target_arch = "m68k")]
global_asm!(include_str!("hooks-dt2.s"));
#[cfg(target_arch = "m68k")]
global_asm!(include_str!("hooks-dn2.s"));

macro_rules! entries {
    ($($(#[$m:meta])* $name:ident,)*) => {
        $(
            #[cfg(target_arch = "m68k")]
            extern "C" {
                $(#[$m])*
                pub fn $name();
            }
            #[cfg(not(target_arch = "m68k"))]
            $(#[$m])*
            pub extern "C" fn $name() {}
        )*
    };
}

entries! {
    // both devices: the machine page's draw, code, key and turn (the stock tail is bound per firmware)
    mf_draw_hook,
    mf_code_hook,
    mf_key_hook,
    mf_turn_hook,
    // Digitakt II
    mf_clone5,
    mf_typearg,
    mf_srcpage,
    // Digitone II
    mf_group,
    mf_typemap0,
    mf_typemap1,
    mf_typemap2,
    mf_frametype,
    mf_reader,
    mf_owner,
}
