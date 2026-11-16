//! Every hook of the machines service as assembler (ColdFire, GNU syntax): `hooks-common.s` (the page entries, one routine for both devices), `hooks-dt2.s` and `hooks-dn2.s`, in one
//! object (a hook is declared for its device only, `module.toml` `devices` and the `service!` call). The files are plain `.s`, included here only, so that the host tests of core/machines
//! assemble them as they are (`tests/test_pk_machines_fw.py`). They read the table `MF_TAB` (`lib.rs`) and call `dks_page_key` and `dks_page_turn` (`fw.rs`).
use core::arch::global_asm;

global_asm!(include_str!("hooks-common.s"));
global_asm!(include_str!("hooks-dt2.s"));
global_asm!(include_str!("hooks-dn2.s"));

extern "C" {
    // both devices: the machine page's draw, code, key and turn (the stock tail is bound per firmware)
    pub fn mf_draw_hook();
    pub fn mf_code_hook();
    pub fn mf_key_hook();
    pub fn mf_turn_hook();
    // Digitakt II
    pub fn mf_clone5();
    pub fn mf_typearg();
    pub fn mf_srcpage();
    // Digitone II
    pub fn mf_group();
    pub fn mf_typemap0();
    pub fn mf_typemap1();
    pub fn mf_typemap2();
    pub fn mf_frametype();
    pub fn mf_reader();
    pub fn mf_owner();
}
