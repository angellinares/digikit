# Running modified firmware

The runtime (browser and desktop) identifies firmware by the SHA-256 of its `.syx`
(`devices/*.toml`). A modified image matches no hash. This page says how such an image
still runs, and how it declares where its own code lives, so the runaway check stays
tight. The diagnostic oracle (`native/boot/src/main.rs`) is unchanged: it runs stock
releases only.

## Which release it was derived from

Section 5 of the ELE3 container (`meta`, 15 bytes, loaded nowhere) holds the release's
build stamp, `260908 14:25:18` for Digitone II 1.11. A modification that leaves section 5
alone still names its release, so each `[[firmware]]` entry records its `build_stamp`.

When no hash matches, `Registry::boot_for_derived` finds the release whose stamp the
meta section contains, and the runtime boots the image on that release's contract,
without the MAIN hash check (MAIN is what was modified). Everything past that point is
found by signature, as for stock. The session reports `modified: true`, and the
faceplate shows `dn2 1.11 (modified)`: a derived run is never reported as the stock
release.

## Where its code may run

Stock firmware executes only its MAIN image (`0x40000400` + its length), so a PC
outside it stops the run as `UnsupportedGuestPc`: a runaway. A modified image may also
run code it copied into RAM at boot. It says where in either of two ways.

**1. In the image (preferred).** A table appended to MAIN, after its last stock byte,
which the image's own boot code reads to place its routines. It's the `DNFW` area of
[dn2_firmware_explore](https://github.com/angellinares/dn2_firmware_explore)'s mod
platform (`src/dnfw/patch/area.py`). All fields are big-endian u32 unless noted:

| offset | field |
|---|---|
| +0 | `'DNFW'` (4 bytes) |
| +4 | total length, header included; the area ends at or before MAIN's end |
| +8 | chunk count |
| +12 | per chunk, 12 bytes: 4-byte id, offset from the area's start, length |

A chunk with id `CODE` starts with its **load address** and **image length** (then
BSS length and init entry, and the image). The runtime lets the PC enter
`[load, load + image length)` of every `CODE` chunk, and nowhere else outside MAIN:
not the chunks' BSS, not their data. Another toolchain can append the same table to
declare its own code. The ranges then travel with the bytes and cannot drift from them.

**2. Declared by the developer (an override).** For code an image places some other
way: a hand patch, or a toolchain that writes no table. In the faceplate, **Exec
ranges** takes `LO-HI` pairs (hex with `0x` or decimal, `LO:HI` also accepted,
separated by commas, semicolons or spaces), e.g. `0x4670c000-0x4670ef48`. It's kept in
the browser and applied before every load and restart. These ranges apply to any
image, stock included, because the developer stated them. The host call is
`digi_exec_ranges(text)`; empty text clears them.

A modified image with neither declaration may run anywhere in the ColdFire's RAM
(`0x40000000..0x48000000`): looser, but it still catches a jump into nowhere. The
firmware's own fault reporter still catches the commonest runaway, an illegal
instruction or address error.
