/* test/file-write-b: the code of test/file-write under another module name and command range (0x28 to 0x2b), so that a test has a second module
   whose calls use a handle of the first. */
#define PROOF_CMD 0x28
#include "../../file-write/src/file-write-proof.c"
