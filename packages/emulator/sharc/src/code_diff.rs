//! Which generated blocks a patch changed (route A of finding 55's emulator cost).
//!
//! `verify_code` hashes the loaded code per range. A range is a merged span of
//! many blocks, so one patched word used to bar every region with an entry in
//! the range. On a range whose hash differs the engine now decodes the loaded
//! words, compares them with the generated instruction table, and bars only
//! the regions with a block that holds a differing PC. These functions hold
//! that logic without any generated data, so unit tests can run them.

use crate::rt::Insn;

/// `CODE_LOOKAHEAD` of tools/sharc_dev/sharc_rsgen.py: the short words after a
/// block that the decoder may read. A block is taken to end that many words
/// after the next block's start (more than it needs: when in doubt, bar more).
pub const LOOKAHEAD: u32 = 6;

/// A range with more differing PCs than this is barred as a whole (a range
/// not loaded yet differs everywhere, and a decode per PC is not worth it).
pub const MAX_CHANGED: usize = 512;

/// An instruction compared as the generated blocks see it: its type, kind,
/// length and field entries (`offset` is always 0).
#[derive(Clone, Copy, Debug)]
pub struct InsnKey(pub Insn);

impl PartialEq for InsnKey {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (&self.0, &other.0);
        a.type_name == b.type_name
            && a.kind == b.kind
            && a.length_bytes == b.length_bytes
            && a.fields.entries() == b.fields.entries()
    }
}

/// The PCs in `start .. start + len` where the generated table has an
/// instruction and `loaded` gives another one, or none (the loaded words do
/// not decode). PCs the table has no instruction at are skipped: no block
/// runs from them, and a change that reaches an instruction through the
/// decoder's lookahead shows at that instruction. None when more than `cap`
/// PCs differ.
pub fn changed_pcs<K: PartialEq>(
    start: u32,
    len: u32,
    generated: impl Fn(u32) -> Option<K>,
    mut loaded: impl FnMut(u32) -> Option<K>,
    cap: usize,
) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for pc in start..start + len {
        let Some(want) = generated(pc) else { continue };
        if loaded(pc).as_ref() != Some(&want) {
            if out.len() >= cap {
                return None;
            }
            out.push(pc);
        }
    }
    Some(out)
}

/// The entry PCs (sorted) that stay off after a code change: those of every
/// region that has a block holding a PC of `changed` (sorted), and those of a
/// region with an entry for which `whole` is true (its range could not be
/// narrowed down). A region's bodies pass control to each other directly, so
/// all its entries go together. A block is `[its start, the next block's
/// start + LOOKAHEAD)`; the block starts are the entries of all regions.
pub fn barred_entries(
    region_entries: &[&[u32]],
    changed: &[u32],
    whole: impl Fn(u32) -> bool,
) -> Vec<u32> {
    let mut starts: Vec<u32> = region_entries
        .iter()
        .flat_map(|e| e.iter().copied())
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let hit = |pc: u32| {
        if whole(pc) {
            return true;
        }
        let i = starts.partition_point(|&s| s < pc);
        let end = starts
            .get(i + 1)
            .map_or(u32::MAX, |&n| n.saturating_add(LOOKAHEAD));
        let first = changed.partition_point(|&c| c < pc);
        changed.get(first).is_some_and(|&c| c < end)
    };
    let mut barred = Vec::new();
    for entries in region_entries {
        if entries.iter().any(|&pc| hit(pc)) {
            barred.extend_from_slice(entries);
        }
    }
    barred.sort_unstable();
    barred.dedup();
    barred
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three blocks of thirty words (starts 0, 30, 60, the last ends at 90),
    /// the regions {0, 60} and {30}. A toy decoder: the instruction at PC is
    /// the three words from PC on (the decoder's lookahead), and every PC is
    /// an instruction.
    const ENTRIES: [&[u32]; 2] = [&[0, 60], &[30]];
    const WORDS: usize = 100;

    fn insn(words: &[u16], pc: u32) -> Option<[u16; 3]> {
        let at = pc as usize;
        (at + 3 <= words.len()).then(|| [words[at], words[at + 1], words[at + 2]])
    }

    /// The entries barred when the loaded words are the stock ones with the
    /// words at PATCHED changed.
    fn barred(patched: &[usize]) -> Vec<u32> {
        let stock: Vec<u16> = (0..WORDS as u16).collect();
        let mut loaded = stock.clone();
        for &p in patched {
            loaded[p] ^= 0x8000;
        }
        let changed = changed_pcs(
            0,
            90 + LOOKAHEAD,
            |pc| insn(&stock, pc),
            |pc| insn(&loaded, pc),
            MAX_CHANGED,
        )
        .unwrap();
        barred_entries(&ENTRIES, &changed, |_| false)
    }

    #[test]
    fn no_patch_bars_nothing() {
        assert_eq!(barred(&[]), Vec::<u32>::new());
    }

    #[test]
    fn a_word_inside_a_block_bars_only_that_blocks_region() {
        // The middle block: region {30} only.
        assert_eq!(barred(&[45]), vec![30]);
        // The last block: the region of blocks 0 and 60 goes together, but
        // not the middle block's region.
        assert_eq!(barred(&[75]), vec![0, 60]);
        // The first block.
        assert_eq!(barred(&[10]), vec![0, 60]);
    }

    #[test]
    fn a_word_in_the_lookahead_bars_the_preceding_block() {
        // Word 60 starts the last block and is read by the instructions at 58
        // and 59 of the middle block: both regions.
        assert_eq!(barred(&[60]), vec![0, 30, 60]);
        // Word 31 is read by the instructions 29 to 31: the first block's
        // region as well as the middle block's.
        assert_eq!(barred(&[31]), vec![0, 30, 60]);
        // Word 62 is read from 60 on; the margin of LOOKAHEAD words after the
        // next start keeps the middle block in.
        assert_eq!(barred(&[62]), vec![0, 30, 60]);
    }

    #[test]
    fn a_range_that_cannot_be_narrowed_bars_its_regions() {
        let changed: Vec<u32> = vec![];
        // The first block's range is whole: its region, not the middle one.
        let barred = barred_entries(&ENTRIES, &changed, |pc| pc < 30);
        assert_eq!(barred, vec![0, 60]);
    }

    #[test]
    fn too_many_changes_give_none() {
        let stock = |pc: u32| Some(pc);
        assert_eq!(changed_pcs(0, 10, stock, |_| None, 3), None);
        assert_eq!(
            changed_pcs(0, 10, stock, |_| None, 10).map(|v| v.len()),
            Some(10)
        );
        // A PC the table has nothing at is skipped, whatever is loaded.
        let sparse = |pc: u32| (pc % 2 == 0).then_some(pc);
        assert_eq!(
            changed_pcs(0, 10, sparse, |_| None, 10).map(|v| v.len()),
            Some(5)
        );
    }

    #[test]
    fn a_word_that_does_not_decode_counts_as_changed() {
        let stock: Vec<u16> = (0..WORDS as u16).collect();
        let changed = changed_pcs(
            0,
            10,
            |pc| insn(&stock, pc),
            |pc| insn(&stock[..5], pc),
            MAX_CHANGED,
        )
        .unwrap();
        assert_eq!(changed, vec![3, 4, 5, 6, 7, 8, 9]);
    }
}
