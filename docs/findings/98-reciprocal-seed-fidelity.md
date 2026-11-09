# Reciprocal seed fidelity: what the public documentation says

**[D]** The inspected public documentation does not specify the RECIPS seed table.
The SHARC+ Programming Reference Rev. 1.5, printed page 20-17 (PDF page 472),
describes a ROM indexed by the seven leading fraction bits, result exponent -e-1,
and preserved input sign. It provides neither 128 mantissa values nor an exact
selection/rounding prescription. An accuracy bound alone does not select the exact
bits.

Primary sources:

- [SHARC+ Programming Reference](https://www.analog.com/media/en/dsp-documentation/processor-manuals/SC58x-2158x-prm.pdf#page=472).
- [Classic SHARC Programming Reference Rev. 2.4](https://www.analog.com/media/en/dsp-documentation/processor-manuals/adsp-2136x_2137x_214xx_pgr_rev2.4.pdf#page=517), printed page 11-41: same bounded description without the table.
- [Applications Handbook, section 2.5.1](https://www.analog.com/media/en/dsp-documentation/legacy-processor-manuals/56546799ADSP_21000_Family_Applications_Handbook_Vol.1.0.pdf#page=54), printed pages 44-45: uses RECIPS as a seed operation rather than specifying its ROM.

Four targeted official-domain searches found no exact table or rounding rule.
This is a bounded search result, not proof that no public specification exists.
No vendor toolchain material was used.

The seed tables of RECIPS and RSQRTS come from a measurement on a Digitakt II, not
from documentation: see [finding 102](102-recips-rsqrts-seed-tables.md).
