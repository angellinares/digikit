# The emulated +Drive: `plus_drive/` and `samples/`

`mise run workbench` builds the emulated +Drive from a folder, `plus_drive/<device>/`
(`plus_drive/dt2/` for the Digitakt II, `plus_drive/dn2/` for the Digitone II),
ignored by git. The device is the one the firmware file belongs to. Use
`mise run workbench -- --plus-drive DIR` for another folder (`--samples DIR` is the same
option, kept for the old `samples/` flow). A folder that is missing or holds nothing
the device can use leaves the emulator on its built-in empty card; `--card-image`
overrides the folder. The image is built by `packages/emulator/plusdrive`
(`plusdrive build`, also `mise run dev:plusdrive -- build DIR -o IMAGE --syx FW`) and cached by
content in `out/plusdrive/native/<key>/<device>.img` (key: firmware, builder
binary, device, every file's relative path and bytes).

## Folder contract

- **Files and folders map to the path filesystem as they are** (DT2). Nested folders,
  empty folders, directories of up to 4,092 entries, files of any size up to the card.
  Symbolic links, `.DS_Store` and non-regular files are reported and skipped.
- **A `.wav` is converted to a native sample**: 48 kHz, 16-bit big-endian PCM, mono or
  stereo, a 0x40 header and a 0x10 trailer. The stored name loses the `.wav` extension
  (case-insensitive), non-ASCII characters become `?`, 255 bytes at most. Two names that
  become equal in one folder are an error.
- **Any other file is copied byte for byte** and keeps its name (same character rule). If
  the bytes pass the native-sample check (the check the builder applies to every sample)
  it is a native sample, otherwise a plain file; plain files are not given to the boot project.
- **`.mmcfs/projects/NNNN.bin`, `.mmcfs/sounds/NNNN.bin`, `.mmcfs/kits/NNNN.bin`**: raw
  slot contents of the three MmcFs pools, NNNN = the slot index (4 decimal digits; projects
  0 to 127, sounds 0 to 2047, kits 0 to 1023). The builder writes each into its fixed
  slot and leaves its write-protect bit clear (the bitmaps at sector 0x800 are padlocks, not occupancy:
  finding 35). A file larger than
  its slot (DT2: project 16 MiB, sound 4 KiB, kit 64 KiB; DN2: 16 MiB, 1 KiB, 32 KiB) is an
  error. Anything else under `.mmcfs/` is reported and skipped.
- **`.mmcfs/boot/SSSSSS.bin`**: a raw 16 MiB region outside the pools; SSSSSS is its first sector in hex (the exporter
  writes these names). The regions come from the device profile (`[[card.regions]]`): the two boot copies
  (`040000`, `048000`) and the working project (`050000`). The builder writes the file at that sector (at most 0x8000
  sectors; another sector is reported and skipped).
- **The boot project** (the COKi project the firmware reads at start, sectors 0x40000 and 0x48000) is generated when the
  folder holds at least one native sample (DT2 with `--syx`): its references point at the samples in id order. **A folder
  file replaces the generated project at its sector**: if `.mmcfs/boot/040000.bin` exists, 0x40000 is that file and the
  generated project is not written there (the same for `048000`; each copy on its own). The project of an earlier session
  refers to sample ids, so it matches only the same sample set: delete the boot file to have the project generated again.
  The working project (`050000`) is written only from a file; the exporter does not write it by default (the firmware
  rewrites it from the boot copy at every start; `--with-working-project` adds it). `.mmcfs/projects/` cannot collide with these regions: slots
  end before them.
- **`digikit/`** (top level only) is not copied to the path filesystem: every file under it, recursively, goes to the
  module store (`docs/design/digikit-store.md`), on DT2 and DN2. More than the area holds (`[card.store]`; 82 MiB on the DT2) is an error.
- **DN2 has no path filesystem**: only `.mmcfs/` and `digikit/` apply; every other file is reported and skipped, and the
  image carries no path filesystem structures. DN2 pool addresses are from the 1.11 decompile.
- **The card layout is device data**: `[card]` in `devices/<device>.toml` (pool base sector, stride, slot count and size,
  bitmap place, project regions, path-filesystem superblock, store range). The builder, reader and exporter take it; a new
  device is a TOML section. `plusdrive ... --device dt2|dn2` names the device by its `short` name.

## Exit export

`mise run workbench` starts the app with `--device-folders`: at every load the app builds the card from the loaded device's folder (`plus_drive/<device>/`, or the folder chosen in the card-folder strip of the Files page) when it holds something the card takes, and treats that folder as the card's +Drive folder: when the app exits, the
firmware's changes to the card are exported back into the folder (files of the path filesystem, `.mmcfs/` slots and
regions; finding 24) and a line says how many files changed:

```
card: exported 2 changed files to plus_drive/dt2 (2 added, 0 replaced; 0 deleted by the firmware, kept; 0 issues); backup out/plusdrive/backup/<stamp>
```

Replaced files are moved to `out/plusdrive/backup/<stamp>/` first. `mise run workbench -- --no-sync` leaves the folder alone on
exit (`card.export` still works by hand). A saved project is 14.5 MB, and the working project is another 14.5 MB, so a
folder grows by that much once the firmware has saved. The exported project changes the folder's content, so the next
start builds a new card image and, with audio, boots its ready state again (the cache key is the folder's content).

The desktop "Sample folder..." button stages the chosen folder (all files, nested folders and `.mmcfs/` included) and builds
the card with the same builder. It does not sync back: the staged copy is only a copy.

## `samples/` (the older flow)

Put WAV files in a local `samples/` directory at the repository root (ignored by git; create it if it is missing).
WAV files there become the emulated Digitakt II's +Drive: `tools/firmware/plusdrive.py
build samples/ -o out/plusdrive/dt2.img` reads every `.wav` file directly in
this folder (not recursively) and writes them into a card image in the
firmware's own +Drive filesystem format
(`docs/findings/14-plus-drive-format.md`), which the emulator can then boot
from (`--card-image out/plusdrive/dt2.img` on the runners that accept it).

**Nothing in `samples/` is committed** (`.gitignore` excludes the
directory), including the two test files below -- generate them
instead of expecting them to already be here:

```
uv run python -m tools.emulator.gen_test_samples
```

That writes:

- `test-sine-1khz.wav` -- a 1 kHz sine, 48 kHz/16-bit/mono, 0.5 s.
- `test-perc-click.wav` -- a short exponentially-decaying click (a low thump
  plus a little noise), same format, 50 ms -- stands in for a percussive
  drum sample without needing a real recording.

Both are fully synthetic and safe to regenerate at any time; add your own
`.wav` files alongside them for a real test.

`tools/firmware/plusdrive.py` converts each input WAV to the drive's native sample
format (the one the firmware's recorder writes and its loader reads): 48 kHz,
16-bit big-endian PCM, mono or stereo, behind a 64-byte header. Other rates
are resampled and other bit depths (8/24/32-bit integer, 32/64-bit float) are
converted; more than two channels is refused.

Unless `--no-project` is given, the image also carries an active project (the
firmware's built-in one, taken from `out/sections/dt2-1.16/` at build time)
whose sample references all point at the first file in name order, so the
firmware loads that file into every track's slot at boot with no UI.
