# Ledger: T2 step 2, card, settings and safe-mode duplicates move to host tests

Status 2026-11-16: the host tests pass (`core/runtime:rust-tests`, merged with main 77f76827c). No device test is deleted yet: each deletion waits for its replacement to pass and for an independent review. Rust paths are under `modules/core/runtime/`.

Line format: `device test | assertion | -> host test` or `KEPT: reason`. TS files are under `tests/`. `FP` is `src/rs/src/fake_platform.rs`.

## card-safety.device.test.ts

Deleted: "a card with our store keeps working", torn cases 2, 3 and 4 (header A torn with B whole, header A torn with B blank, selection copy A torn).

| Device test | Assertion | Host test |
| --- | --- | --- |
| a card with our store keeps working | the check says ours, nothing refused | FP `a_valid_selection_record_makes_the_area_ours_without_scanning_it` (state, `first_nonblank`, five reads, no claim) |
| a card with our store keeps working | a selection write and an upload still work (no refusal, no claim) | FP same test (a data write after the check: OK, `claims` 0); `card.rs` `our_card_allows_every_write_in_range` |
| a card with our store keeps working | `bootInfo` count 1, loaded 1, safe 0 | KEPT: `safe-mode.device.test.ts` "REC released" (count 1, loaded 1, safe 0; lines 155, 160-161) |
| torn 2: header A torn by the claim's second write, B whole | state ours | FP `the_card_state_after_a_torn_claim_or_selection_write` (case "header A torn, B whole") |
| torn 2 | an upload commits | `sdk/src/store/tests.rs:607-627` (host, a store with a valid header B and a lagging A); `upload-faults.device.test.ts:77` (full-tier device cut "after header B, before A") |
| torn 3: header A torn, B blank | foreign, first non-blank sector 0 | FP same test (case "header A torn, B blank") |
| torn 3 | BEGIN and CLEAR answer E_INUSE, nothing written | FP `a_foreign_card_names_its_first_nonblank_sector_and_hash_and_refuses_selection_and_range` (E_CARD_RO for every write, nothing on the card); the E_INUSE mapping stays device-tested in "foreign bytes in the range make the card read-only" |
| torn 4: selection copy A torn, rest blank | state blank | FP same torn test (case "selection copy A torn") |
| torn 4 | the next selection writes both copies | KEPT: `card-safety.device.test.ts:82-85` (selection write on a blank card writes both copies; a card with torn copy A and the rest blank is a blank card, FP torn test). Nothing on the host runs `selection_set` |

New host cases with no device twin (they cover what the kept tests assert through the real adapter): every refused write outside the range is counted (`every_write_outside_the_store_is_refused_for_range_and_counted_whatever_its_size`, the eight sizes of "a blank card works as before"); a foreign card names its sector and hash and refuses selection, probe and range writes; a claim cut after its first write leaves a card the next check calls ours (`a_claim_cut_after_its_first_write_leaves_a_card_the_next_boot_calls_ours`, the host side of "an upload on a blank card claims the range first").

KEPT on the device (real boundaries): "a blank card works as before ..." (card-write function behind `block_write`, CARD_CHECK and CARD_REFUSED log events, the mailbox probe), "a selection record only ..." (E_NOSTORE of the store handlers on foreign headers), "an upload on a blank card claims the range first" (the cut knob and the next boot), "foreign bytes ... read-only" (INFO, LIST, ABOUT rows, E_INUSE), torn 1 (E_NOSTORE, CLEAR recovery, upload), `card-small` (the firmware's block read refuses past the capacity).

## settings-copies.device.test.ts

Deleted: table rows `higher` and `both` of "copies/$name". Row `torn` stays: it reads the boot event, the error flag, the value and the next write through the real card.

| Device row | Assertion | Host test |
| --- | --- | --- |
| higher | boot reads copy A (seq 9 over 3): state 1, copy 0, seq 9, no error flag | `sdk/src/settings/tests.rs` `the_higher_seq_wins_and_a_equal_pair_takes_a` (copy, seq, damaged 0); `src/rs/src/settings.rs` `the_load_state_says_none_loaded_or_damaged` (state 1) |
| higher | get returns the value of A | `sdk/src/settings/tests.rs` `the_higher_seq_wins_and_a_equal_pair_takes_a` (entries) |
| higher | the next write goes to copy B with seq 10, A untouched | `settings.rs` `the_next_write_goes_to_the_other_copy_with_the_next_seq`; `sdk/src/settings/tests.rs` `a_write_takes_only_the_sectors_the_record_needs_and_only_its_copy` |
| both | no valid copy: state 0 (both copies damaged), copy none, seq 0, error flag | `sdk/src/settings/tests.rs` `both_copies_damaged_is_an_empty_record` (copy NONE, damaged 2); `settings.rs` `the_load_state_says_none_loaded_or_damaged` (state 0). The error flag is the one condition `damaged != 0` in `load`; the SDK test asserts `damaged` 2 for this case, and the kept `torn` row asserts the flag on the device |
| both | get answers E_SET_NOKEY | `sdk/src/settings/tests.rs` `both_copies_damaged_is_an_empty_record` (no entries), `put_get_replace_remove` (E_SET_NOKEY on an absent key) |
| both | the next write goes to copy A with seq 1 | `settings.rs` `the_next_write_goes_to_the_other_copy_with_the_next_seq` (case NONE) |

## safe-mode.device.test.ts

Deleted: cases `A new / B old` and `A torn / B torn` of "power cut, A x / B y" (full tier only). Cases `A torn / B old` and `A old / B new` stay (torn CRC through the real boot, fallback to B, no module loaded at seq 1). The smoke cases (REC held and released) and the other files' tests are not changed.

| Device case | Assertion | Host test |
| --- | --- | --- |
| A new / B old | boot uses A, seq 2, copy 0, all modules load | `selection.rs` `a_torn_copy_is_bad_and_the_boot_takes_the_valid_one_with_the_higher_seq` (use, 2, 0) and `the_choice_over_every_pair_of_copy_states`; loading from copy A is kept by "one copy written by the runtime" |
| A old / B new | KEPT | the only device case that loads modules from a copy-B record |
| A torn / B torn | no valid copy: loaded 0, seq 0, copy 0 | same torn test (empty) |
| all | modules load from the chosen record | KEPT: case `A torn / B old` (loaded 0 at seq 1 shows the boot follows the choice) and "one copy written by the runtime ..." (loaded all, copy 0) |

New host case with no device twin: a copy cut inside its sector write (second half zero) is bad.

## Not changed, with the reason

- `settings.device.test.ts`, `settings-cards.device.test.ts`: main, restart, blank, overlap and oldcard are the mailbox, the 2 s timer through emulated time and the card boot paths (real boundaries). `write_due` and `next_write` are host-tested now, but the timer through the firmware clock stays a device case.
- `settings-api.device.test.ts` test 3 (key rules through the mailbox): not an exact duplicate (audit). Not touched.
- DSP mode: `card-safety` and the settings tests already run `dsp: 'zero'`. `safe-mode` runs `dsp: 'sharc'`; the fault-module cases (reset in init, safe-once flag, hang early and late) load no DSP module and may run on zero. [O] one proof run decides; the power-cut and REC cases load a machine or `c-demo` and stay as they are until then.

## Blank scan range (checked)

`check` scans `BLANK_SECTORS` = 2048 sectors from the area start. Header B is at relative sector `sectors - TAIL`, with `TAIL = 1 + 2 * 16 + 2 = 35`, and selection copy A at `sectors - 2`. The host layout has 4096 sectors: header B at 4061, selection A at 4094, both beyond 2048, so a torn header B or selection A is not seen by the scan and the card reads blank, as on the device (real areas are far larger). Header A (sector 0) is inside the scan.
