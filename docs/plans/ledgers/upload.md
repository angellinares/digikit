# Ledger: the `upload` device scenarios (tools/patchkit/pk_digikit_upload.py)

One line per Python check: `scenario | U:line | what | -> TS file:test name | or COVERED / DROPPED / NOT PORTED`.
Files are under `tests/regression/runtime/`. Names: `U` = `upload.test.ts`, `F` = `upload-faults.test.ts`, `K` = `upload-kit.ts` (protocol bodies, `uploadTests`, `roundtrip`; the rig is `card-rig.ts`).
Tests: Q = `upload-quick: one upload, a replace and a delete` (DT2, always), O = `upload-ops: the commands, errors and CLEAR` (full),
FO = `upload-folders: folders, ranges, a streamed upload, first fit and paging` (full), ST = `upload-stock: ...` (full), TX = `upload-txfull: ...` (full),
SC = `upload-screenshot: ...` (always), CUT = `upload-cut: ...` (full), CC = `upload-clear-cut: ...` (full), PB = `upload-playback: ...` (DT2, full).

Rig differences from Python (all scenarios): one reusable runtime daemon per test and device (`UploadRig`), a cold boot per card through
`reloadRuntimeBaseline` on a clone of the card, paused and stepped by the protocol transport (`stepInstructions` 400,000) as the Python `EmuTransport`.
The Python `common_checks_upload` is `UploadRig.finish`: write-watch events 0, flush hits 0, at least one emulator, and every overlay sector in the store range or at or above the working project sector.
Python `c.parallel` cases run one after the other in one daemon.

## quick (U:154-196, 243-253), DT2

| scenario | U | what | TS |
| --- | --- | --- | --- |
| quick | 162 | INFO: protocol 1, runtime version, device, free bytes of an empty store, 32 KiB chunks, not safe, no upload open | U:Q |
| quick | 173 | LIST of a card with no store: empty | U:Q |
| quick | 183 | put a 70,000-byte file | U:Q |
| quick | 184 | first upload: LIST = files sent, card read by plusdrive = bytes sent, store version 3 | U:Q |
| quick | 185 | READ checks R1-R9 (see ops) | U:Q |
| quick | 250 | replace: LIST, card, version | U:Q |
| quick | 253 | delete: LIST, card, version | U:Q |
| quick | 194 | the upload wrote no selection sector | U:Q |
| quick | 195 | common_checks_upload | K:UploadRig.finish |
| quick | 50-54 | DN2 DKRT_STORE_CAP poke (not DT2 unless the store is over 4 GiB) | K:UploadRig.applyCap |
| quick | 193 | report fields `device_requests`, `emu_instructions_stepped` | DROPPED: instruction and request counts are report data, not checks |

## READ and VERIFY (U:198-240), in quick and ops

| scenario | U | what | TS |
| --- | --- | --- | --- |
| ops | 201 | READ: first 0xa0 bytes = file | U:Q, O (readChecks) |
| ops | 205 | READ: a range across a sector boundary, and the file size | U:Q, O |
| ops | 211 | READ: a long request returns 768 bytes | U:Q, O |
| ops | 217 | READ: a range past the end is cut at the end | U:Q, O |
| ops | 219 | READ: the whole file by 768-byte reads | U:Q, O |
| ops | 220 | VERIFY: device sha256 = local | U:Q, O |
| ops | 224 | VERIFY: a missing file refused (-59) | U:Q, O |
| ops | 230 | READ: missing file -59, offset at the end -54, length 0 -55 | U:Q, O |

## ops (U:256-371, 374-418, 421), full tier, DT2 and DN2

| scenario | U | what | TS |
| --- | --- | --- | --- |
| ops | 267 | put a/z.bin 513, modules/b.dkm 512, modules/c.dkm 1, empty 0, modules/big.dkm 65,536 bytes | U:O |
| ops | 268 | six files: LIST, card, version | U:O |
| ops | 272 | replace | U:O |
| ops | 277 | DELETE of a missing path: E_NOFILE | U:O |
| ops | 278 | delete | U:O |
| ops | 282 | BEGIN past the free bytes: E_FULL with the free bytes | U:O |
| ops | 289 | BEGIN of one sector is accepted | U:O |
| ops | 290 | INFO: an upload is open | U:O |
| ops | 292 | ABORT closes it | U:O |
| ops | 296 | a wrong sha256: E_HASH | U:O (putWithHash) |
| ops | 297 | an upload ended by END is closed | U:O |
| ops | 298 | after E_HASH nothing committed (LIST, card) | U:O |
| ops | 304 | a DATA retry (same offset, same length) is ok | U:O |
| ops | 308 | DATA at an offset neither next nor a retry: E_OFFSET | U:O |
| ops | 314 | DATA not a multiple of 512 and not the last: E_LENGTH | U:O |
| ops | 320 | DATA past the file's size: E_LENGTH | U:O |
| ops | 322 | DATA with no bytes: E_MSG or E_LENGTH | U:O |
| ops | 324 | END before all bytes: E_LENGTH | U:O |
| ops | 329 | the rest of the file after the retry | U:O |
| ops | 332 | after the retry (LIST, card) | U:O |
| ops | 334 | DATA with no upload open: E_STATE | U:O |
| ops | 336 | END with no upload open: E_STATE | U:O |
| ops | 342 | BEGIN with 7 bad paths: E_PATH | U:O |
| ops | 344 | a short BEGIN: E_MSG | U:O |
| ops | 346 | CLEAR without its key: E_MSG | U:O |
| ops | 348 | another protocol version: E_PROTO (raw request, version byte 2) | U:O (UploadRig.request) |
| ops | 350 | an unknown command: E_PROTO | U:O |
| ops | 351 | nothing above changed the store | U:O |
| ops | 358 | CLEAR (LIST, card) | U:O |
| ops | 360 | CLEAR gives the space back | U:O |
| ops | 367 | BEGIN of exactly the free bytes is accepted | U:O |
| ops | 371 | an upload after CLEAR (LIST, card) | U:O |
| ops | 394 | RT1 +Drive round trip: export writes the store's files to digikit/ | K:roundtrip (in O) |
| ops | 400 | RT2 a second export adds, replaces and keeps nothing | K:roundtrip (in O) |
| ops | 414 | RT3 the next card build holds the uploaded files | K:roundtrip (in O) |
| ops | 422 | the upload wrote no selection sector | U:O |
| ops | 160-171 | INFO and empty LIST at the start | U:O |
| ops | put+verify of small modules, LIST after reboot, CLI delete/persistence | | COVERED by live.ts installModules, tests/regression/modules/acceptance.ts, transfer-live.test.ts (the ops file puts are the protocol-level cases, kept) |

## folders (U:458-697), full tier, DT2 and DN2

| scenario | U | what | TS |
| --- | --- | --- | --- |
| folders | 469 | INFO: capability FOLDERS | U:FO |
| folders | 474 | CAPACITY: a STORE entry, area size in KiB, used 0, largest 0 | U:FO |
| folders | 511 | LIST under samples/ of an empty store: nothing | U:FO |
| folders | 517 | two MKDIR (nested; mkdir twice is ok): LIST = files, card, folder entries | U:FO |
| folders | 523 | MKDIR a//b, ../x, a/: E_PATH | U:FO |
| folders | 525 | MKDIR with an empty path: E_MSG | U:FO |
| folders | 527 | MKDIR with no NUL: E_MSG or E_PATH | U:FO |
| folders | 529 | LIST with a prefix and no NUL: E_MSG | U:FO |
| folders | 554 | cost of 1 MiB (instructions): DATA, END, VERIFY | DROPPED: instruction counts |
| folders | 560 | END of a 1 MiB file takes far less than its DATA | DROPPED: instruction counts |
| folders | 572 | put a 6 MiB file under a folder entry | U:FO |
| folders | 578 | INFO: free bytes fell by the file's sectors and the table room | U:FO |
| folders | 584 | CAPACITY: STORE used grew by the file in KiB | U:FO |
| folders | 589 | big upload: LIST, card, folder entry gone | U:FO |
| folders | 592 | VERIFY by 1 MiB ranges equals the host's | U:FO (dk.verifyFile) |
| folders | 596 | VERIFY of the last, short range | U:FO |
| folders | 610 | VERIFY range (100,10), (len+512,1), (512,0), (512,len) refused | U:FO |
| folders | 616 | delete: LIST, card | U:FO |
| folders | 617 | the delete gave the free bytes back | U:FO |
| folders | 625 | CAPACITY: STORE used fell again | U:FO |
| folders | 628 | a file of the same size: LIST, card | U:FO |
| folders | 629 | the new file took the first gap (starts at or before the deleted one) | U:FO |
| folders | 639 | three files in a row | U:FO |
| folders | 645 | a smaller file after a delete in the middle | U:FO |
| folders | 647 | the smaller file went into the hole between f0 and f2 | U:FO |
| folders | 658 | MKDIR of a file and inside a file: E_PATH | U:FO |
| folders | 662 | MKDIR of an implied folder makes no entry | U:FO |
| folders | 664 | MKDIR tmp/x | U:FO |
| folders | 666 | BEGIN over a folder entry: E_PATH | U:FO |
| folders | 668 | DELETE of an implied folder: E_NOFILE | U:FO |
| folders | 674 | DELETE of a folder entry | U:FO |
| folders | 680 | forty small files: LIST, card | U:FO |
| folders | 682 | LIST under many/: forty entries in several pages | U:FO |
| folders | 689 | LIST under a prefix nobody has: nothing | U:FO |
| folders | 690 | LIST with the prefix `m` finds many/ only | U:FO |
| folders | 694 | +Drive round trip RT1-RT3 | K:roundtrip (in FO) |
| folders | 697 | common_checks_upload | K:UploadRig.finish |
| folders | 532-553 | the 1 MiB probe uploads and deletes (they only fed the instruction counts) | DROPPED with the two cost checks above |
| folders | 542-543, 553, 695 | report fields | DROPPED: report data |

Folders compares the card by `plusdrive store ls` size and sha256 (as Python `card_ls`), not by file bytes; the bytes are read only in the round trip.

## cut (U:703-775), full tier, DT2 and DN2

Five cases (cuts after write 1, 3, 4, 5, 6), each on two cold boots (the cut card, then the card as the cut leaves it).

| scenario | U | what | TS |
| --- | --- | --- | --- |
| cut | 733 | the upload ends ok only when all six writes are allowed, otherwise it is refused | F:CUT |
| cut | 750 | the next boot reads the old store (cuts 1, 3, 4) or the new store (5, 6), whole | F:CUT |
| cut | 756 | plusdrive reads the same files from the card | F:CUT |
| cut | 765 | the next upload works and commits | F:CUT |
| cut | 775 | common_checks_upload | K:UploadRig.finish |

## stock (U:795-846), full tier

| scenario | U | what | TS |
| --- | --- | --- | --- |
| stock | 835 | the stock firmware answered the five MidiRpc requests | U:ST |
| stock | 841 | Ping, SoftwareVersion, DeviceUID replies byte-identical with and without the runtime (and a foreign F0 7D message goes the stock way) | U:ST |
| stock | 846 | common_checks_upload | K:UploadRig.finish |

Difference: the stock reference is the unpatched firmware started from the CF snapshot (`Session.start`, no runtime), not booted on the same card image.

## playback (U:1095-1162), DT2, full tier, coupled DSP

| scenario | U | what | TS |
| --- | --- | --- | --- |
| playback | 1132 | the module was uploaded during the note | F:PB |
| playback | 1139 | the note is audible (peak above 0.01) | F:PB |
| playback | 1141 | PCM during the upload = PCM alone, from the onset on, onset shift even and at most 256 samples | F:PB (K:pcmMatches, K:alignedCompare) |
| playback | 1156 | the added card writes are store sectors only, none is a selection sector | F:PB |
| playback | 1162 | common_checks_upload | K:UploadRig.finish |

Differences: the two runs are two cold boots in one daemon (not two parallel daemons); the trigger frame is the later of 3500 and the setup margin, and the second run keeps the first run's frame when it can. The machine is selected with `selectSine` (fx-session.ts), not `rig.select_type` and `dyn_type`.

## clear-cut (U:1194-1307), full tier, DT2 and DN2

| scenario | U | what | TS |
| --- | --- | --- | --- |
| clear-cut | 1259 | CLEAR cut after header B is refused its second write (-57) | F:CC |
| clear-cut | 1266 | after the cut the next boot reads B, the empty store | F:CC |
| clear-cut | 1276 | the upload whose header B tears reports a write error | F:CC |
| clear-cut | 1281 | clear-cut-then-tear: store reads, empty, plusdrive agrees, the next upload commits | F:CC |
| clear-cut | 1291 | upload cut after header B: refused its last write | F:CC |
| clear-cut | 1301 | the second upload's header B tears | F:CC |
| clear-cut | 1302 | upload-cut-then-tear: old + first upload still there, plusdrive agrees, the next upload commits | F:CC |
| clear-cut | 1307 | common_checks_upload | K:UploadRig.finish |

## txfull (U:1336-1402), full tier, DT2 and DN2

| scenario | U | what | TS |
| --- | --- | --- | --- |
| txfull | 1349 | the in-flight word (anchor `cf.midi.site.tx_inflight_lit` of the port file) is a DDR address | U:TX |
| txfull | 1354 | an INFO answers while the ring is empty | U:TX |
| txfull | 1371 | a full ring and 512 bytes in flight: 20 requests each, no reply reaches the output | U:TX (usbSend, usbTake) |
| txfull | 1376 | the 40 replies were dropped and counted | U:TX |
| txfull | 1385 | the MIDI task still routes stock messages: the Ping answers while the ring is full | U:TX |
| txfull | 1394 | 256 bytes in flight and an empty ring: the reply is sent | U:TX |
| txfull | 1402 | common_checks_upload | K:UploadRig.finish |

Raw USB delivery uses the control methods `midi.usb.in` and `midi.usb.out` directly (the same calls `EmuUsbTransport` makes); no new infrastructure.

## screenshot (U:1405-1430), DT2 and DN2, always

| scenario | U | what | TS |
| --- | --- | --- | --- |
| screenshot | 1418 | Screenshot reply (main): 1024 bytes equal to the frame capture before or after | U:SC |
| screenshot | 1423 | the screen is not blank (main) | U:SC |
| screenshot | 1418 | same after tapping SETTINGS | U:SC |
| screenshot | 1423 | not blank after SETTINGS | U:SC |
| screenshot | 1417 | PNG file of the shot | DROPPED: the raw OLED bytes are saved as `.oled` in the artifacts |
| screenshot | 1430 | common_checks_upload | K:UploadRig.finish |

## rust (U:880-1089) and the Rust binary build (U:852-877)

| scenario | U | what | TS |
| --- | --- | --- | --- |
| rust | 880-1089 | the whole upload-rust scenario (Rust host Session against the device side) | DROPPED: stays Python for now (decided) |
