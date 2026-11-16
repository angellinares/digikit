//! The role of a `.dkm` or `.dks` file (docs/plans/runtime-and-services.md section 13.2): the role word of the header and what the header table may hold for it.
//! Not wired into the boot yet (R12-3 loads the providers).
#![allow(dead_code)]
use dkneeds::keys::{tag, Keys};
use dkrt_sdk::E_FORMAT;

/// The role word of a file: 0 a module, 1 a service (`.dks`). `head` is the first 64 bytes of the file; the word is big endian, like every header field.
pub fn role_of(head: &[u8; 64]) -> u32 {
    let at = dkneeds::H_ROLE as usize;
    u32::from_be_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]])
}

/// Whether the header table `keys` fits the role: a module (0) has no PROVIDES, HOOKS or BIND item; a service (1) has table version 2 and exactly one
/// PROVIDES item. Another role is `E_FORMAT`, like the other refusals.
pub fn role_ok(role: u32, keys: &Keys) -> Result<(), i32> {
    if role == 0 {
        // No packer writes HOOKS for a module (patchkit major3.rs writes only USES and KEYS, dkm.rs adds NEEDS): HOOKS are a service's (role 1).
        if keys.count(tag::PROVIDES) != 0 || keys.count(tag::HOOKS) != 0 || keys.count(tag::BIND) != 0 {
            return Err(E_FORMAT);
        }
        Ok(())
    } else if role == 1 {
        if keys.version() != 2 || keys.count(tag::PROVIDES) != 1 {
            return Err(E_FORMAT);
        }
        Ok(())
    } else {
        Err(E_FORMAT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dkneeds::keys::{write_table, Pair, Table};
    use std::vec;
    use std::vec::Vec;

    const KEEP: Table<'static> =
        Table { needs: &[], uses: &[], keys: &[], kinds: &[], slots: &[], hooks: &[], exports: &[], resources: &[], provides: &[], bind: &[] };

    fn bytes(t: &Table) -> Vec<u8> {
        let mut b = vec![0u8; dkneeds::keys::table_size(t)];
        write_table(t, &mut b).unwrap();
        b
    }

    fn check(role: u32, t: &Table) -> Result<(), i32> {
        let b = bytes(t);
        role_ok(role, &Keys::parse(&b).unwrap())
    }

    const ONE: &[Pair] = &[Pair { a: 7, b: 0x010000 }];

    #[test]
    fn the_role_word_is_read_big_endian_from_the_header() {
        let mut head = [0u8; 64];
        assert_eq!(role_of(&head), 0);
        head[dkneeds::H_ROLE as usize + 3] = 1;
        assert_eq!(role_of(&head), 1);
        head[dkneeds::H_ROLE as usize] = 1;
        assert_eq!(role_of(&head), 0x0100_0001);
    }

    #[test]
    fn a_module_with_nothing_for_a_service_is_ok() {
        assert_eq!(check(0, &KEEP), Ok(()));
    }

    #[test]
    fn a_module_with_a_provides_hooks_or_bind_item_is_refused() {
        assert_eq!(check(0, &Table { provides: ONE, ..KEEP }), Err(E_FORMAT));
        assert_eq!(check(0, &Table { hooks: &[Pair { a: 9, b: 4 }], ..KEEP }), Err(E_FORMAT));
    }

    #[test]
    fn a_service_needs_version_2_and_exactly_one_provides_item() {
        assert_eq!(check(1, &Table { provides: ONE, ..KEEP }), Ok(()));
        assert_eq!(check(1, &KEEP), Err(E_FORMAT));
        assert_eq!(check(1, &Table { provides: &[Pair { a: 7, b: 0 }, Pair { a: 8, b: 0 }], ..KEEP }), Err(E_FORMAT));
        // a table of version 1
        let mut b = bytes(&Table { provides: ONE, ..KEEP });
        b[4..6].copy_from_slice(&1u16.to_be_bytes());
        if let Ok(k) = Keys::parse(&b) {
            assert_eq!(role_ok(1, &k), Err(E_FORMAT));
        }
    }

    #[test]
    fn another_role_is_refused() {
        assert_eq!(check(2, &Table { provides: ONE, ..KEEP }), Err(E_FORMAT));
        assert_eq!(check(u32::MAX, &KEEP), Err(E_FORMAT));
    }
}
