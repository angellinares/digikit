# ADSP-2156x data addressing evidence

Primary source: Analog Devices,
[ADSP-21562/21563/21565/21566/21567/21569 datasheet, Rev. D, June 2023](https://www.analog.com/media/en/technical-documentation/data-sheets/adsp-21562-21563-21565-21566-21567-21569.pdf),
Tables 2, 3 and 6, printed pages 9–10. Checked 2026-10-02.

Table 6 gives DMC0 normal-word data addresses `0x10000000..0x17ffffff`
for byte-addressed DDR starting at `0x80000000`. Tables 2–3 also distinguish
private L1 and L2 byte/normal-word aliases. These are address mappings,
not evidence that uninitialized memory reads zero.

Application to this repository: strict final-entry startup stops at a
Type 3c normal-word read of `0x10000000`; section 7 fills DDR at `0x80000000`.
This is a missing address-translation candidate. The current shared memory
helper receives address and width, but no architectural access-space argument.
It also serves byte and short accesses. A safe correction must preserve that
context, translate normal-word accesses only and use the corresponding DAG
modifier units. It must cover Rust's interpreter and AOT memory fast paths,
plus canonical overlay writes and watchpoints.

Before enabling corrected addressing, validate distinct nonzero adjacent DDR
words, both normal-word endpoints, byte/short accesses at the same numeric
addresses, coherent writes through aliases and Python/native instruction
comparisons. A zero-fill-only continuation would conceal incorrect scaling.
The product memory map also replaces the current PRM-likely-only ACONV shift
assumption where it differs; ILAD behavior and unsupported spaces remain
separate concerns.

The earlier INIT routine additionally needs ROM call context, advancing
EMUCLK and clock-generator status transitions. These dependencies do not
justify seeding arbitrary running state or treating a captured-state render
as fresh-boot audio.
