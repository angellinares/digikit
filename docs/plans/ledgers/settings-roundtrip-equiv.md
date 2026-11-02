# Ledger: settings, roundtrip and equiv scenarios ported to TypeScript

Status 2026-11-02: the Python scenarios, wrappers and pk_digikit_settings.py are deleted; the module equiv TOMLs and test_equiv_*.py stay.

Line format: `scenario | file:line | what | -> TS file:test | or COVERED by <test> | or DROPPED/NOT PORTED: reason`.
Python files: `S` = `6a5bb1ed:tools/patchkit/pk_digikit_settings.py`, `P` = `tools/patchkit/pk_digikit_project.py`, `C` = `tools/patchkit/pk_digikit_scen.py`, `M` = `tools/patchkit/pk_dkrt_machine.py`.
TS files are `modules/core/runtime/tests/settings.device.test.ts`, `modules/core/adapters/dynreg/tests/roundtrip.device.test.ts` and `equiv.device.test.ts` (below named without the `.device` part), helpers in `project-kit.ts`, fixture in `card-rig.ts`); the encoder test is `tests/regression/host/settings-record.test.ts`.
Every TypeScript daemon is coupled (ColdFire and DSP). The Python `cf_only` engine has no TypeScript counterpart; the tiers and devices of the TOML files are kept.

## settings-dn2, settings-dt2 (quick, both devices, coupled)

settings | S:125 | main: boot read the settings area, no valid copy (state 0, copy none, seq 0, 18 bytes, no entry) | -> settings.test.ts: main
settings | S:132 | main: CAPACITY SETTINGS limit 8192 bytes, 18 used, INFO has the SETTINGS capability | -> settings.test.ts: main
settings | S:144 | main: SETTINGS answers E_SET_NOKEY while there is no record | -> settings.test.ts: main
settings | S:153 | main: put and get give the values back (route 1 byte, amounts 16 bytes) | -> settings.test.ts: main
settings | S:164 | main: missing key E_SET_NOKEY; key with a space E_SET_KEY for put, get, remove | -> settings.test.ts: main
settings | S:169 | main: the same value again changes nothing | -> settings.test.ts: main
settings | S:174 | main: CAPACITY used = 18 + 29 + 43 | -> settings.test.ts: main
settings | S:180 | main: nothing is written in the first second | -> settings.test.ts: main
settings | S:187 | main: SETTINGS gives the RAM record (flag 1, copy none) with the two entries | -> settings.test.ts: main
settings | S:199 | main: 1.5 s after the second change still nothing is written | -> settings.test.ts: main
settings | S:206 | main: about 2 s after the last change one card write, copy A only | -> settings.test.ts: main
settings | S:213 | main: SETTINGS says copy A, seq 1, nothing waiting, last values | -> settings.test.ts: main
settings | S:222 | main: copy A on the card is a valid record, 90 bytes, zero after | -> settings.test.ts: main (decodeSettings from project-kit.ts, vectors checked in host/settings-record.test.ts)
settings | S:232 | main: `patchkit store dump` (text and JSON) decodes the keys, copy A active, B empty | -> settings.test.ts: main
settings | S:251 | main: next change goes to copy B (seq 2); A byte for byte unchanged | -> settings.test.ts: main
settings | S:264 | main: after a soft restart the boot reads copy B (state 1, copy 1, seq 2, 2 entries); values are the last ones | -> settings.test.ts: main. The restart is the USB RESTART (`dk.restart`), not the mailbox CMD_RESTART: see the report
settings | S:273 | main: SETTINGS after the restart: copy B, seq 2, nothing waiting | -> settings.test.ts: main
settings | S:281 | main: a removed key is gone at once | -> settings.test.ts: main
settings | S:289 | main: next write goes to copy A (seq 3) with the new key and without the removed; B unchanged | -> settings.test.ts: main
settings | S:299 | main: the only store sectors written are the two settings copies | -> settings.test.ts: main
settings | S:312 | restart: right after a put the change is not on the card (flag bit 0, no settings write) | -> settings.test.ts: restart
settings | S:320 | restart: restart at once keeps the put (state 1, copy 0, seq 1, 1 entry, value there) | -> settings.test.ts: restart
settings | S:329 | restart: only settings copy A is written besides the selection record | -> settings.test.ts: restart
settings | S:343 | blank: first write claims the blank area (header pair, claims 1) and writes copy A | -> settings.test.ts: blank
settings | S:353 | blank: `store dump` shows an empty store and the key in copy A | -> settings.test.ts: blank
settings | S:372 | copies/torn,higher,both: boot reads the right copy (state, copy, seq), error flag, get value | -> settings.test.ts: copies/$name (table of three)
settings | S:386 | copies/torn,higher,both: next write goes to the right copy with seq + 1; the other copy untouched | -> settings.test.ts: copies/$name
settings | S:419 | overlap: older store with a file in the settings area: SETTINGS_LOAD state 4, error event, E_BAD_STORE | -> settings.test.ts: overlap
settings | S:428 | overlap: CAPACITY SETTINGS limit 0, used 0 | -> settings.test.ts: overlap
settings | S:439 | overlap: put, get, remove and SETTINGS answer E_SET_OFF | -> settings.test.ts: overlap
settings | S:449 | overlap: LIST answers E_NOSTORE | -> settings.test.ts: overlap
settings | S:456 | overlap: nothing written in the store range; the four file sectors unchanged | -> settings.test.ts: overlap
settings | S:482 | oldcard: older header B at sectors - 3 boots, module loads, no safe mode, selection kept | -> settings.test.ts: oldcard
settings | S:492 | oldcard: settings store on (state 0, limit 8192), store lists its file | -> settings.test.ts: oldcard
settings | S:502 | oldcard: settings write goes to copy A only (besides the selection sectors) | -> settings.test.ts: oldcard
settings | S:512 | oldcard: an upload writes header B at the new place with the same seq; both files listed | -> settings.test.ts: oldcard
settings | S:520 | oldcard: `store dump`: two files, selection kept, key in copy A | -> settings.test.ts: oldcard
settings | C:406 | common: write-watch on cf.expansion over every boot, no stray write | -> card-rig.ts cardRig afterClose (runs after the daemon closes)
settings | C:406 | common: flush check over every boot, no hit | -> card-rig.ts cardRig afterClose
settings | C:413 | common: store range writes are the two selection sectors plus store_extra (settings copies, header pair, oldcard writes) | -> card-rig.ts cardRig afterClose (`storeExtra`, `allowStoreWrites`)
settings | C:422 | common: card writes elsewhere only from 0x50000 | -> card-rig.ts cardRig afterClose
settings | S:121 (encode) | tools/device/dksettings.py encode and decode against sdk/vectors | -> host/settings-record.test.ts (valid and invalid vectors, limits)
settings | tests/digikit/scenarios/settings-*.toml `cf_only = false` | coupled boot for the 2 s count | kept: all TS daemons are coupled
settings | S:530 | the parts run in parallel threads on separate daemons | DROPPED: one test per part in one reusable daemon; reload gives each part its own card

## roundtrip-dn2 (full), roundtrip-dt2 (quick), roundtrip-dt2-orders (full), roundtrip-dt2-coupled (full)

roundtrip | P:359 | boot k: every module loaded, no safe mode (ColdFire part, results 0 or LOADING) | DROPPED: cf engine only; the coupled variant below replaces it
roundtrip | P:267 | boot k: coupled boot check, loaded == selection, safe 0, all results 0 (thread) | -> roundtrip.test.ts: a project passes through the module sets (checked for every boot)
roundtrip | P:370 | boot k: every module loaded (coupled engine) | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:302 | set X: machine keeps its type number (DKRT_ASSIGN) in later boots | -> roundtrip.test.ts: a project passes through the module sets (dynType in project-kit.ts)
roundtrip | P:185 | DT2: track holds the wanted type after trying the list positions | -> roundtrip.test.ts: buildProject
roundtrip | P:411 | control: the panel script changed the project (first save differs from the untouched one, pattern bytes) | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:418 | control: pattern 1 gained trig bytes and lock data | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:423 | control: sounds changed (kit or pool bytes) | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:430 | first save holds a sound with each run-time machine type | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:438 | each later save equals the first but for the header stamp | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:446 | saves 3 and 4 are byte-identical to the second save | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:453 | sound records still carry each type after every boot | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:460 | the header stamp changes once and then stays | -> roundtrip.test.ts: a project passes through the module sets
roundtrip | P:465 | common checks (write-watch, flush, store writes, writes elsewhere) | -> card-rig.ts cardRig afterClose (policy default, no storeExtra)
roundtrip | roundtrip-dt2-coupled.toml | same case as roundtrip-dt2 on the coupled engine | COVERED by roundtrip.test.ts: roundtrip-dt2 (every TS daemon is coupled); the full-tier entry is not repeated
roundtrip | roundtrip-dt2.toml `tier = "quick"` | quick DT2 case ran on a ColdFire-only daemon (4x faster) | NOT PORTED as cf-only: runs coupled, so it is slower than before
roundtrip | roundtrip-dt2-orders.toml | DT2 case (full tier): load orders A, A, B, A2 of the same sets as roundtrip-dt2 | -> roundtrip.test.ts: roundtrip-dt2-orders (case table, same test body as the other cases)
roundtrip | P:355 (c.used_cards, DIGI_COLD_RELAX_FS) | a card the firmware saved on skips the DT2 filesystem check | -> card-rig.ts cardRig `relaxFs` (daemon env DIGI_COLD_RELAX_FS=1)

## equiv-dn2-set (quick, dn2)

equiv | C:665 | selection loaded at boot: every module result 0, loaded == count, safe 0 | -> equiv.test.ts: the rows equal the static twin
equiv | C:685 | voice rows claimed == 2 | -> equiv.test.ts: the rows equal the static twin (voiceRows in project-kit.ts)
equiv | C:719 | machine, parameter and page rows equal the static twin's (ignore Sine) | -> equiv.test.ts: the rows equal the static twin (decodeTables, diffTables)
equiv | C:726 | the run-time set added rows | -> equiv.test.ts: the rows equal the static twin
equiv | M:503 | pcm_pin: onset even, 24000 samples, SHA-256 equals the pinned 330e9725...db1d3 | -> equiv.test.ts: the rows equal the static twin. Em accepted the TS pin (2026-11-02).
equiv | C:653 | reference note audible | DROPPED: runs only when no pcm_sha256 pin is set; this scenario has the pin
equiv | M:466 | pcm_check run against reference PCM | DROPPED: runs only without a pin
equiv | C:751 | common checks | -> card-rig.ts cardRig afterClose (both daemons)
equiv | C:674 | report of boot_ready (frames_at_ready, voice rows at ready) | DROPPED: report fields, no assertion
