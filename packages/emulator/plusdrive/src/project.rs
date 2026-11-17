//! DT2 1.16 built-in-project extraction and reference replacement.

use device_profile::PlusdriveProjectContract;
use dt2_firmware_loader::depack_section;

use crate::build_project_header;

pub const PROJECT_RECORD_SIZE: usize = 0x00dd_9714;
const PROJECT_HEADER_SIZE: usize = 0x110;
const BUILTIN_STREAM_OFFSET: usize = 0x25edd8;
const MAX_DEPACKED_BYTES: usize = 0x00c3_f424;
const CONTAINER_SIZE: usize = 0x00c3_f424;
const CONTAINER_MAGIC: u32 = 0xbeef_bace;
const CONTAINER_END_OFFSET: usize = 0x00c3_e400;
const CONTAINER_END_MAGIC: u32 = 0xbace_f00c;
const REF_TABLE_OFFSET: usize = 0x00c2_d6fb;
const REF_COUNT: usize = 1024;
const KITS_OFFSET: usize = 0x00ae_0200;
const KIT_SIZE: usize = 0x2800;
const KIT_HEADER: usize = 0x3c;
const TRACK_SIZE: usize = 0x155;
const TRACK_SLOT: usize = 0x64;
const TRACK_REF: usize = 0x129;
/// Kit 0 of the built-in project is a factory demo kit; kit 32 is an
/// untouched default kit, which a card-less boot's kit matches.
const ACTIVE_KIT: usize = 0;
const DEFAULT_KIT: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct ProjectSeed<'a> {
    pub main: &'a [u8],
    pub contract: PlusdriveProjectContract,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectReport {
    pub table_refs: usize,
    pub extra_refs: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectError {
    UnsupportedContract,
    StreamOutOfBounds,
    InvalidStreamHeader,
    InvalidStreamChecksum,
    Depack(String),
    InvalidContainer,
    InvalidReference,
    OutputOverflow,
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .and_then(|part| part.try_into().ok())
        .map(u32::from_be_bytes)
}

/// Whether `main` holds a stream at `at`: a plausible length, then a byte sum
/// that matches the checksum word.
fn is_stream(main: &[u8], at: usize) -> bool {
    let (Some(length), Some(sum)) = (word(main, at), word(main, at + 4)) else {
        return false;
    };
    let length = length as usize;
    if !(0x1000..=0x10_0000).contains(&length) {
        return false;
    }
    main.get(at + 8..at + 8 + length).is_some_and(|payload| {
        payload
            .iter()
            .fold(0u32, |total, byte| total.wrapping_add(u32::from(*byte)))
            == sum
    })
}

/// Where the built-in project stream starts in MAIN. 1.16 has it at
/// `BUILTIN_STREAM_OFFSET`; a relinked build (1.17) moves it with the same
/// bytes, so look for the stream by structure (length, byte sum, and a
/// depacked container that validates) and fall back to the 1.16 offset.
fn stream_offset(main: &[u8]) -> usize {
    let valid = |at: usize| {
        is_stream(main, at)
            && main.get(at..).is_some_and(|stream| {
                depack_section(stream, at, MAX_DEPACKED_BYTES)
                    .is_ok_and(|container| validate_container(&container).is_ok())
            })
    };
    if valid(BUILTIN_STREAM_OFFSET) {
        return BUILTIN_STREAM_OFFSET;
    }
    (0..main.len().saturating_sub(8))
        .step_by(4)
        .find(|&at| is_stream(main, at) && valid(at))
        .unwrap_or(BUILTIN_STREAM_OFFSET)
}

fn extract(seed: ProjectSeed<'_>) -> Result<Vec<u8>, ProjectError> {
    if seed.contract != PlusdriveProjectContract::Dt2V3Default116 {
        return Err(ProjectError::UnsupportedContract);
    }
    let at = stream_offset(seed.main);
    let stream = seed.main.get(at..).ok_or(ProjectError::StreamOutOfBounds)?;
    let length = word(stream, 0).ok_or(ProjectError::InvalidStreamHeader)? as usize;
    let sum = word(stream, 4).ok_or(ProjectError::InvalidStreamHeader)?;
    let end = 8usize
        .checked_add(length)
        .ok_or(ProjectError::InvalidStreamHeader)?;
    let payload = stream.get(8..end).ok_or(ProjectError::StreamOutOfBounds)?;
    if payload
        .iter()
        .fold(0u32, |total, byte| total.wrapping_add(u32::from(*byte)))
        != sum
    {
        return Err(ProjectError::InvalidStreamChecksum);
    }
    let container = depack_section(stream, at, MAX_DEPACKED_BYTES)
        .map_err(|error| ProjectError::Depack(error.to_string()))?;
    validate_container(&container)?;
    Ok(container)
}

fn validate_container(container: &[u8]) -> Result<(), ProjectError> {
    if container.len() != CONTAINER_SIZE
        || word(container, 0) != Some(CONTAINER_MAGIC)
        || word(container, 4) != Some(3)
        || word(container, CONTAINER_END_OFFSET) != Some(CONTAINER_END_MAGIC)
    {
        return Err(ProjectError::InvalidContainer);
    }
    Ok(())
}

fn nonempty_refs(container: &[u8]) -> Result<Vec<[u8; 16]>, ProjectError> {
    let end = REF_TABLE_OFFSET
        .checked_add(REF_COUNT * 16)
        .ok_or(ProjectError::InvalidContainer)?;
    let table = container
        .get(REF_TABLE_OFFSET..end)
        .ok_or(ProjectError::InvalidContainer)?;
    let mut refs = Vec::new();
    for &reference in table.as_chunks::<16>().0 {
        if reference != [0; 16]
            && reference != [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        {
            refs.push(reference);
        }
    }
    Ok(refs)
}

/// The slot of each of the 16 tracks of the active kit (the int16 at
/// `TRACK_SLOT`), `None` when it is out of the reference table.
fn track_slots(container: &[u8]) -> [Option<usize>; 16] {
    let mut slots = [None; 16];
    for (track, slot) in slots.iter_mut().enumerate() {
        let at = KITS_OFFSET + ACTIVE_KIT * KIT_SIZE + KIT_HEADER + track * TRACK_SIZE + TRACK_SLOT;
        if let Some(value) = container.get(at..at + 2) {
            let value = i16::from_be_bytes([value[0], value[1]]);
            *slot = usize::try_from(value)
                .ok()
                .filter(|v| (1..REF_COUNT).contains(v));
        }
    }
    slots
}

fn is_used(reference: &[u8; 16]) -> bool {
    *reference != [0; 16]
        && *reference != [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// Gives each of the 16 tracks of the active kit its own table slot: the
/// slot it already uses if no lower track has it, else the lowest unclaimed
/// slot that holds a reference (then any unclaimed slot).
fn claim_slots(container: &[u8]) -> [usize; 16] {
    let used = |slot: usize| {
        let at = REF_TABLE_OFFSET + slot * 16;
        container
            .get(at..at + 16)
            .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
            .is_some_and(|reference| is_used(&reference))
    };
    let mut claimed = [0usize; 16];
    for (track, own) in track_slots(container).into_iter().enumerate() {
        let taken = claimed;
        let free = |slot: &usize| !taken[..track].contains(slot);
        claimed[track] = own
            .filter(free)
            .or_else(|| (1..REF_COUNT).find(|slot| used(*slot) && free(slot)))
            .or_else(|| (1..REF_COUNT).find(free))
            .unwrap_or(1);
    }
    claimed
}

/// One input file: every reference, in the table and in the copies elsewhere
/// in the container, becomes `reference`; the tracks keep their slots. This
/// is the card the one-sample tools (`tools/emulator/dt2_sample_check.py`, `--dt2-card`)
/// were pinned on, so it stays byte-identical.
fn point_all_refs_at(
    mut container: Vec<u8>,
    reference: [u8; 16],
    old: Vec<[u8; 16]>,
) -> (Vec<u8>, ProjectReport) {
    let mut table_refs = 0;
    for slot in 0..REF_COUNT {
        let at = REF_TABLE_OFFSET + slot * 16;
        if is_used(&container[at..at + 16].try_into().unwrap()) {
            container[at..at + 16].copy_from_slice(&reference);
            table_refs += 1;
        }
    }
    let mut distinct = old;
    distinct.sort_unstable();
    distinct.dedup();
    let mut copies = 0;
    for old_ref in distinct {
        let mut at = 0;
        while let Some(found) = container[at..]
            .windows(16)
            .position(|candidate| candidate == old_ref)
        {
            let position = at + found;
            container[position..position + 16].copy_from_slice(&reference);
            copies += 1;
            at = position + 16;
        }
    }
    (
        container,
        ProjectReport {
            table_refs,
            extra_refs: copies,
        },
    )
}

/// Points the project's sample references at `references` (the input files
/// in sorted order). Track t of the active kit gets its own table slot and
/// plays `references[t % n]`; every other reference, in the table and in
/// the copies elsewhere in the container, becomes `references[0]`. With one
/// file nothing is remapped (`point_all_refs_at`).
fn point_refs_at(
    mut container: Vec<u8>,
    references: &[[u8; 16]],
) -> Result<(Vec<u8>, ProjectReport), ProjectError> {
    let first = *references.first().ok_or(ProjectError::InvalidReference)?;
    let old = nonempty_refs(&container)?;
    if old.is_empty() {
        return Ok((
            container,
            ProjectReport {
                table_refs: 0,
                extra_refs: 0,
            },
        ));
    }
    if references.len() == 1 {
        return Ok(point_all_refs_at(container, first, old));
    }
    let claimed = claim_slots(&container);
    let table = REF_TABLE_OFFSET..REF_TABLE_OFFSET + REF_COUNT * 16;
    let mut distinct = old;
    distinct.sort_unstable();
    distinct.dedup();
    let mut copies = 0;
    for old_ref in distinct {
        let mut at = 0;
        while let Some(found) = container[at..]
            .windows(16)
            .position(|candidate| candidate == old_ref)
        {
            let position = at + found;
            if !table.contains(&position) {
                container[position..position + 16].copy_from_slice(&first);
                copies += 1;
            }
            at = position + 16;
        }
    }
    let mut table_refs = 0;
    for slot in 0..REF_COUNT {
        let at = REF_TABLE_OFFSET + slot * 16;
        if is_used(&container[at..at + 16].try_into().unwrap()) {
            container[at..at + 16].copy_from_slice(&first);
            table_refs += 1;
        }
    }
    for (track, slot) in claimed.into_iter().enumerate() {
        let reference = references[track % references.len()];
        let at = REF_TABLE_OFFSET + slot * 16;
        if !is_used(&container[at..at + 16].try_into().unwrap()) {
            table_refs += 1;
        }
        container[at..at + 16].copy_from_slice(&reference);
        let kit = KITS_OFFSET + ACTIVE_KIT * KIT_SIZE + KIT_HEADER + track * TRACK_SIZE;
        container[kit + TRACK_SLOT..kit + TRACK_SLOT + 2]
            .copy_from_slice(&(slot as i16).to_be_bytes());
        container[kit + TRACK_REF..kit + TRACK_REF + 16].copy_from_slice(&reference);
    }
    Ok((
        container,
        ProjectReport {
            table_refs,
            extra_refs: copies,
        },
    ))
}

/// Replaces the active kit (after its header) with the default kit, keeping
/// every track's sample slot and reference copy. Run after `point_refs_at`.
fn reset_active_kit(container: &mut [u8]) {
    let dst = KITS_OFFSET + ACTIVE_KIT * KIT_SIZE;
    let src = KITS_OFFSET + DEFAULT_KIT * KIT_SIZE;
    let mut keep: Vec<(usize, Vec<u8>)> = Vec::new();
    for track in 0..16 {
        let at = KIT_HEADER + track * TRACK_SIZE;
        for (field, len) in [(TRACK_SLOT, 2), (TRACK_REF, 16)] {
            let from = dst + at + field;
            keep.push((from, container[from..from + len].to_vec()));
        }
    }
    container.copy_within(src + KIT_HEADER..src + KIT_SIZE, dst + KIT_HEADER);
    for (at, data) in keep {
        container[at..at + data.len()].copy_from_slice(&data);
    }
}

pub fn build_project_record(
    seed: ProjectSeed<'_>,
    references: &[[u8; 16]],
    sequence: u32,
) -> Result<(Vec<u8>, ProjectReport), ProjectError> {
    if references.is_empty() || references.contains(&[0; 16]) {
        return Err(ProjectError::InvalidReference);
    }
    let source = extract(seed)?;
    let (mut container, report) = point_refs_at(source, references)?;
    reset_active_kit(&mut container);
    let mut record = build_project_header(PROJECT_HEADER_SIZE, sequence, 4)
        .map_err(|_| ProjectError::OutputOverflow)?;
    record.extend_from_slice(&container);
    if record.len() > PROJECT_RECORD_SIZE {
        return Err(ProjectError::OutputOverflow);
    }
    record.resize(PROJECT_RECORD_SIZE, 0);
    Ok((record, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in project stream is found in the relinked 1.17 (where it
    /// moves by +0x53c).
    #[test]
    fn finds_the_builtin_stream_in_relinked_firmware() {
        let main_of = |file: &str| {
            let syx = std::fs::read(format!("../../../firmware/{file}")).ok()?;
            let parsed = dt2_firmware_loader::parse(&syx).ok()?;
            parsed
                .sections
                .into_iter()
                .find(|s| s.id == 3)
                .map(|s| s.bytes)
        };
        let Some(b) = main_of("Digitakt_II_OS1.17.syx") else {
            return;
        };
        assert_eq!(stream_offset(&b), BUILTIN_STREAM_OFFSET + 0x53c);
    }
    #[test]
    fn malformed_streams_fail_before_depacking() {
        let seed = ProjectSeed {
            main: &[0; BUILTIN_STREAM_OFFSET + 8],
            contract: PlusdriveProjectContract::Dt2V3Default116,
        };
        assert!(matches!(extract(seed), Err(ProjectError::Depack(_))));
        assert_eq!(
            extract(ProjectSeed {
                main: &[],
                contract: PlusdriveProjectContract::Dt2V3Default116
            }),
            Err(ProjectError::StreamOutOfBounds)
        );
        let mut bad_sum = vec![0; BUILTIN_STREAM_OFFSET + 9];
        bad_sum[BUILTIN_STREAM_OFFSET..BUILTIN_STREAM_OFFSET + 4]
            .copy_from_slice(&1u32.to_be_bytes());
        bad_sum[BUILTIN_STREAM_OFFSET + 8] = 1;
        assert_eq!(
            extract(ProjectSeed {
                main: &bad_sum,
                contract: PlusdriveProjectContract::Dt2V3Default116
            }),
            Err(ProjectError::InvalidStreamChecksum)
        );
    }
    #[test]
    fn validates_container_and_replaces_only_known_refs() {
        let mut container = vec![0; CONTAINER_SIZE];
        container[..4].copy_from_slice(&CONTAINER_MAGIC.to_be_bytes());
        container[4..8].copy_from_slice(&3u32.to_be_bytes());
        container[CONTAINER_END_OFFSET..CONTAINER_END_OFFSET + 4]
            .copy_from_slice(&CONTAINER_END_MAGIC.to_be_bytes());
        let old = [1u8; 16];
        container[REF_TABLE_OFFSET..REF_TABLE_OFFSET + 16].copy_from_slice(&old);
        container[100..116].copy_from_slice(&old);
        let (mut patched, report) = point_refs_at(container, &[[2; 16]]).unwrap();
        // One input file: the tracks keep their slots.
        assert_eq!(report.table_refs, 1);
        assert_eq!(report.extra_refs, 1);
        assert_eq!(&patched[100..116], &[2; 16]);
        patched[4..8].copy_from_slice(&2u32.to_be_bytes());
        assert_eq!(
            validate_container(&patched),
            Err(ProjectError::InvalidContainer)
        );
    }

    fn main_os() -> Option<Vec<u8>> {
        let path = std::env::var_os("DT2_SYX")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .ancestors()
                    .map(|dir| dir.join("firmware").join("Digitakt_II_OS1.17.syx"))
                    .find(|path| path.exists())
            })?;
        let bytes = std::fs::read(path).ok()?;
        let firmware = dt2_firmware_loader::parse(&bytes).ok()?;
        firmware
            .sections
            .into_iter()
            .find(|section| section.id == 3)
            .map(|section| section.bytes)
    }
    #[test]
    fn track_n_plays_file_n_on_the_real_project() {
        let Some(main) = main_os() else { return };
        let refs: Vec<[u8; 16]> = (1..=3u8).map(|n| [n; 16]).collect();
        let seed = ProjectSeed {
            main: &main,
            contract: PlusdriveProjectContract::Dt2V3Default116,
        };
        let source = extract(seed).unwrap();
        assert!(track_slots(&source).iter().all(Option::is_some));
        let (record, report) = build_project_record(seed, &refs, 4).unwrap();
        assert_eq!(record.len(), PROJECT_RECORD_SIZE);
        assert!(report.table_refs > 0 && report.extra_refs > 0);
        let container = &record[PROJECT_HEADER_SIZE..];
        let slots = track_slots(container);
        let mut seen = Vec::new();
        for (track, slot) in slots.iter().enumerate() {
            let slot = slot.unwrap();
            assert!(!seen.contains(&slot), "track {track} shares slot {slot}");
            seen.push(slot);
            let at = REF_TABLE_OFFSET + slot * 16;
            let want = refs[track % 3];
            assert_eq!(container[at..at + 16], want, "table, track {track}");
            let copy =
                KITS_OFFSET + ACTIVE_KIT * KIT_SIZE + KIT_HEADER + track * TRACK_SIZE + TRACK_REF;
            assert_eq!(container[copy..copy + 16], want, "copy, track {track}");
        }
        // Every other used slot holds the first file.
        for slot in 1..REF_COUNT {
            let at = REF_TABLE_OFFSET + slot * 16;
            let entry: [u8; 16] = container[at..at + 16].try_into().unwrap();
            assert!(!is_used(&entry) || refs.contains(&entry), "slot {slot}");
            if is_used(&entry) && !seen.contains(&slot) {
                assert_eq!(entry, refs[0], "slot {slot}");
            }
        }
        eprintln!("track slots {slots:?}");
    }

    #[test]
    fn unused_tracks_take_unclaimed_used_slots_first() {
        let mut container = vec![0; CONTAINER_SIZE];
        container[..4].copy_from_slice(&CONTAINER_MAGIC.to_be_bytes());
        container[4..8].copy_from_slice(&3u32.to_be_bytes());
        for (slot, fill) in [(5usize, 0x11u8), (6, 0x22), (7, 0x33)] {
            let at = REF_TABLE_OFFSET + slot * 16;
            container[at..at + 16].copy_from_slice(&[fill; 16]);
        }
        // Track 0 uses slot 6, track 1 slot 5; the rest use slot 0 (none).
        for (track, slot) in [(0usize, 6i16), (1, 5)] {
            let at = KITS_OFFSET + KIT_HEADER + track * TRACK_SIZE + TRACK_SLOT;
            container[at..at + 2].copy_from_slice(&slot.to_be_bytes());
        }
        let refs = [[0xa; 16], [0xb; 16]];
        let (out, _) = point_refs_at(container, &refs).unwrap();
        let entry = |slot: usize| &out[REF_TABLE_OFFSET + slot * 16..][..16];
        assert_eq!(entry(6), &refs[0]);
        assert_eq!(entry(5), &refs[1]);
        // Track 2 has no slot and takes the unclaimed used slot 7.
        assert_eq!(entry(7), &refs[0]);
    }

    #[test]
    fn shared_slots_are_split_between_tracks() {
        let mut container = vec![0; CONTAINER_SIZE];
        for slot in [5usize, 6] {
            let at = REF_TABLE_OFFSET + slot * 16;
            container[at..at + 16].copy_from_slice(&[slot as u8; 16]);
        }
        for track in 0..3usize {
            let at = KITS_OFFSET + KIT_HEADER + track * TRACK_SIZE + TRACK_SLOT;
            container[at..at + 2].copy_from_slice(&5i16.to_be_bytes());
        }
        let claimed = claim_slots(&container);
        assert_eq!(&claimed[..3], &[5, 6, 1]);
        let mut all = claimed.to_vec();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 16);
    }
}
